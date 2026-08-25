use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

use map_core::TileCoordinate;

use crate::{
    DiskTileCache, HttpTileClient, MemoryTileCache, TileData, TileError, TileFetchResult,
    TileFetcher, TileProvider,
};

/// Priority values used by the scheduler. Higher values run first.
pub struct TilePriority;

impl TilePriority {
    pub const PREFETCH: i32 = 10;
    pub const NEARBY: i32 = 50;
    pub const VISIBLE: i32 = 100;
}

/// Identifies one viewport state. A new viewport generation invalidates queued
/// work from earlier generations and lets consumers discard late results.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct TileGeneration(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileSource {
    Memory,
    Disk,
    Network,
    Revalidated,
}

pub struct TileResult {
    pub tile: TileCoordinate,
    pub generation: TileGeneration,
    pub source: TileSource,
    pub result: Result<Arc<TileData>, TileError>,
}

#[derive(Clone, Debug)]
struct QueuedRequest {
    tile: TileCoordinate,
    generation: TileGeneration,
    priority: i32,
    sequence: u64,
}

impl PartialEq for QueuedRequest {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl Eq for QueuedRequest {}

impl Ord for QueuedRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            // Newer requests win ties. This matters when the viewport keeps
            // changing while all visible tiles have the same priority.
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

impl PartialOrd for QueuedRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct QueueState {
    queue: BinaryHeap<QueuedRequest>,
    queued: HashMap<TileCoordinate, QueuedRequest>,
    active: HashMap<TileCoordinate, TileGeneration>,
    current_generation: TileGeneration,
    next_sequence: u64,
    max_queued_requests: usize,
    shutting_down: bool,
}

struct SchedulerInner<P, F>
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    provider: Arc<P>,
    fetcher: Arc<F>,
    memory: MemoryTileCache,
    disk: Option<DiskTileCache>,
    queue: Mutex<QueueState>,
    wake_workers: Condvar,
    results_tx: mpsc::Sender<TileResult>,
    results_rx: Mutex<mpsc::Receiver<TileResult>>,
}

/// A provider-independent boundary for the presentation layer.
pub trait TileService: Send + Sync {
    fn begin_viewport(&self) -> TileGeneration;
    fn request(&self, tile: TileCoordinate, priority: i32, generation: TileGeneration) -> bool;
    fn try_recv(&self) -> Option<TileResult>;
    fn attribution(&self) -> &str;
}

pub const DEFAULT_MAX_QUEUED_REQUESTS: usize = 512;

/// Background tile scheduler with bounded worker concurrency and a bounded,
/// generation-aware request queue.
pub struct TileScheduler<P, F = HttpTileClient>
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    inner: Arc<SchedulerInner<P, F>>,
    worker_handles: Option<Vec<JoinHandle<()>>>,
    workers_available: bool,
}

impl<P, F> TileScheduler<P, F>
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    pub fn new(
        provider: P,
        fetcher: F,
        memory_capacity: usize,
        disk: Option<DiskTileCache>,
        worker_count: usize,
    ) -> Self {
        Self::new_with_queue_limit(
            provider,
            fetcher,
            memory_capacity,
            disk,
            worker_count,
            DEFAULT_MAX_QUEUED_REQUESTS,
        )
    }

    pub fn new_with_queue_limit(
        provider: P,
        fetcher: F,
        memory_capacity: usize,
        disk: Option<DiskTileCache>,
        worker_count: usize,
        max_queued_requests: usize,
    ) -> Self {
        let (results_tx, results_rx) = mpsc::channel();
        let inner = Arc::new(SchedulerInner {
            provider: Arc::new(provider),
            fetcher: Arc::new(fetcher),
            memory: MemoryTileCache::new(memory_capacity),
            disk,
            queue: Mutex::new(QueueState {
                queue: BinaryHeap::new(),
                queued: HashMap::new(),
                active: HashMap::new(),
                current_generation: TileGeneration::default(),
                next_sequence: 0,
                max_queued_requests: max_queued_requests.max(1),
                shutting_down: false,
            }),
            wake_workers: Condvar::new(),
            results_tx,
            results_rx: Mutex::new(results_rx),
        });

        let worker_count = worker_count.max(1);
        let mut worker_handles = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let worker_inner = Arc::clone(&inner);
            let name = format!("gpuimap-tile-{worker_index}");
            if let Ok(handle) = thread::Builder::new()
                .name(name)
                .spawn(move || worker_loop(worker_inner))
            {
                worker_handles.push(handle);
            }
        }

        Self {
            inner,
            workers_available: !worker_handles.is_empty(),
            worker_handles: Some(worker_handles),
        }
    }

    pub fn begin_viewport(&self) -> TileGeneration {
        let Ok(mut state) = self.inner.queue.lock() else {
            return TileGeneration::default();
        };
        state.current_generation = TileGeneration(state.current_generation.0.wrapping_add(1));
        state.queue.clear();
        state.queued.clear();
        let generation = state.current_generation;
        drop(state);
        self.inner.wake_workers.notify_all();
        generation
    }

    pub fn current_generation(&self) -> TileGeneration {
        self.inner
            .queue
            .lock()
            .map(|state| state.current_generation)
            .unwrap_or_default()
    }

    /// Requests work in the current viewport generation. This method remains
    /// convenient for non-UI callers; presentation code should pass the
    /// generation returned by `begin_viewport`.
    pub fn request(&self, tile: TileCoordinate, priority: i32) -> bool {
        self.request_for_generation(self.current_generation(), tile, priority)
    }

    pub fn request_for_generation(
        &self,
        generation: TileGeneration,
        tile: TileCoordinate,
        priority: i32,
    ) -> bool {
        if !self.workers_available || !tile.is_valid() || tile.zoom > self.inner.provider.max_zoom()
        {
            return false;
        }

        let Ok(state) = self.inner.queue.lock() else {
            return false;
        };
        if state.shutting_down
            || state.current_generation != generation
            || state.active.get(&tile).copied() == Some(generation)
        {
            return false;
        }
        drop(state);

        let namespace = self.inner.provider.cache_namespace();
        if let Some(data) = self
            .inner
            .memory
            .get_fresh(namespace, tile, SystemTime::now())
        {
            let Ok(state) = self.inner.queue.lock() else {
                return false;
            };
            if state.shutting_down
                || state.current_generation != generation
                || state.active.get(&tile).copied() == Some(generation)
            {
                return false;
            }
            drop(state);
            return self
                .inner
                .results_tx
                .send(TileResult {
                    tile,
                    generation,
                    source: TileSource::Memory,
                    result: Ok(data),
                })
                .is_ok();
        }

        let Ok(mut state) = self.inner.queue.lock() else {
            return false;
        };
        if state.shutting_down
            || state.current_generation != generation
            || state.active.get(&tile).copied() == Some(generation)
        {
            return false;
        }

        if let Some(existing) = state.queued.get(&tile).cloned() {
            if existing.priority >= priority && existing.generation == generation {
                return false;
            }
            remove_queued(&mut state, tile);
        }

        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        let request = QueuedRequest {
            tile,
            generation,
            priority,
            sequence,
        };

        if state.queued.len() >= state.max_queued_requests {
            let Some(victim) = state
                .queued
                .values()
                .min_by(|left, right| {
                    left.priority
                        .cmp(&right.priority)
                        .then_with(|| left.sequence.cmp(&right.sequence))
                })
                .map(|request| request.tile)
            else {
                return false;
            };
            let victim_request = state
                .queued
                .get(&victim)
                .expect("victim came from queued values");
            if victim_request.priority > request.priority {
                return false;
            }
            remove_queued(&mut state, victim);
        }

        state.queued.insert(tile, request.clone());
        state.queue.push(request);
        drop(state);
        self.inner.wake_workers.notify_one();
        true
    }

    pub fn try_recv(&self) -> Option<TileResult> {
        self.inner.results_rx.lock().ok()?.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<TileResult> {
        self.inner
            .results_rx
            .lock()
            .ok()?
            .recv_timeout(timeout)
            .ok()
    }

    pub fn memory_cache(&self) -> MemoryTileCache {
        self.inner.memory.clone()
    }

    pub fn provider(&self) -> &P {
        self.inner.provider.as_ref()
    }

    pub fn pending_count(&self) -> usize {
        self.inner
            .queue
            .lock()
            .map(|state| state.queued.len() + state.active.len())
            .unwrap_or(0)
    }
}

impl<P> TileScheduler<P, HttpTileClient>
where
    P: TileProvider,
{
    pub fn new_http(
        provider: P,
        user_agent: impl Into<String>,
        memory_capacity: usize,
        disk: Option<DiskTileCache>,
        worker_count: usize,
    ) -> Result<Self, TileError> {
        let fetcher = HttpTileClient::new(user_agent)?;
        Ok(Self::new(
            provider,
            fetcher,
            memory_capacity,
            disk,
            worker_count,
        ))
    }

    pub fn new_http_with_queue_limit(
        provider: P,
        user_agent: impl Into<String>,
        memory_capacity: usize,
        disk: Option<DiskTileCache>,
        worker_count: usize,
        max_queued_requests: usize,
    ) -> Result<Self, TileError> {
        let fetcher = HttpTileClient::new(user_agent)?;
        Ok(Self::new_with_queue_limit(
            provider,
            fetcher,
            memory_capacity,
            disk,
            worker_count,
            max_queued_requests,
        ))
    }
}

impl<P, F> TileService for TileScheduler<P, F>
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    fn begin_viewport(&self) -> TileGeneration {
        TileScheduler::begin_viewport(self)
    }

    fn request(&self, tile: TileCoordinate, priority: i32, generation: TileGeneration) -> bool {
        self.request_for_generation(generation, tile, priority)
    }

    fn try_recv(&self) -> Option<TileResult> {
        TileScheduler::try_recv(self)
    }

    fn attribution(&self) -> &str {
        self.provider().attribution()
    }
}

impl<P, F> Clone for TileScheduler<P, F>
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            worker_handles: None,
            workers_available: self.workers_available,
        }
    }
}

impl<P, F> Drop for TileScheduler<P, F>
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    fn drop(&mut self) {
        let Some(worker_handles) = self.worker_handles.take() else {
            return;
        };
        if let Ok(mut state) = self.inner.queue.lock() {
            state.shutting_down = true;
            // Queued requests are obsolete once the owner is being dropped;
            // workers are allowed to finish only the requests already in
            // flight.
            state.queue.clear();
            state.queued.clear();
            state.active.clear();
        }
        self.inner.wake_workers.notify_all();
        for handle in worker_handles {
            let _ = handle.join();
        }
    }
}

fn remove_queued(state: &mut QueueState, tile: TileCoordinate) {
    state.queued.remove(&tile);
    state.queue = state.queued.values().cloned().collect();
}

fn worker_loop<P, F>(inner: Arc<SchedulerInner<P, F>>)
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    loop {
        let request = {
            let Ok(mut state) = inner.queue.lock() else {
                return;
            };
            loop {
                while state.queue.is_empty() && !state.shutting_down {
                    let Ok(next_state) = inner.wake_workers.wait(state) else {
                        return;
                    };
                    state = next_state;
                }
                if state.shutting_down {
                    return;
                }
                let Some(candidate) = state.queue.pop() else {
                    continue;
                };
                let Some(current) = state.queued.get(&candidate.tile) else {
                    continue;
                };
                if current.sequence != candidate.sequence {
                    continue;
                }
                state.queued.remove(&candidate.tile);
                state.active.insert(candidate.tile, candidate.generation);
                break candidate;
            }
        };

        let namespace = inner.provider.cache_namespace();
        let (source, result) = resolve_request(&inner, request.tile, namespace);

        let shutting_down = inner
            .queue
            .lock()
            .map(|state| state.shutting_down)
            .unwrap_or(true);
        if shutting_down {
            clear_active(&inner, request.tile, request.generation);
            continue;
        }

        if let Ok(data) = &result
            && matches!(source, TileSource::Network | TileSource::Revalidated)
        {
            inner
                .memory
                .insert(namespace, request.tile, Arc::clone(data));
            if let Some(disk) = &inner.disk
                && data.metadata.is_storeable()
            {
                // A full or read-only cache must not make a successfully
                // downloaded tile disappear from the UI.
                let _ = disk.insert(request.tile, data);
            }
        }

        let _ = inner.results_tx.send(TileResult {
            tile: request.tile,
            generation: request.generation,
            source,
            result,
        });
        clear_active(&inner, request.tile, request.generation);
    }
}

fn clear_active<P, F>(
    inner: &SchedulerInner<P, F>,
    tile: TileCoordinate,
    generation: TileGeneration,
) where
    P: TileProvider,
    F: TileFetcher<P>,
{
    if let Ok(mut state) = inner.queue.lock()
        && state.active.get(&tile).copied() == Some(generation)
    {
        state.active.remove(&tile);
    }
}

fn resolve_request<P, F>(
    inner: &SchedulerInner<P, F>,
    tile: TileCoordinate,
    namespace: &str,
) -> (TileSource, Result<Arc<TileData>, TileError>)
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    let now = SystemTime::now();
    if let Some(data) = inner.memory.get(namespace, tile) {
        if data.metadata.is_fresh(now) {
            return (TileSource::Memory, Ok(data));
        }
        return fetch_from_network(inner, tile, Some(data));
    }

    if let Some(disk) = &inner.disk
        && let Ok(Some(data)) = disk.get(tile)
    {
        let data = Arc::new(data);
        if data.metadata.is_fresh(now) {
            inner.memory.insert(namespace, tile, Arc::clone(&data));
            return (TileSource::Disk, Ok(data));
        }
        return fetch_from_network(inner, tile, Some(data));
    }
    fetch_from_network(inner, tile, None)
}

fn fetch_from_network<P, F>(
    inner: &SchedulerInner<P, F>,
    tile: TileCoordinate,
    cached: Option<Arc<TileData>>,
) -> (TileSource, Result<Arc<TileData>, TileError>)
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    match inner
        .fetcher
        .fetch_with_cache(&inner.provider, tile, cached.as_deref())
    {
        Ok(TileFetchResult::Updated(data)) => (TileSource::Network, Ok(Arc::new(data))),
        Ok(TileFetchResult::NotModified(metadata)) => {
            let Some(cached) = cached else {
                return (
                    TileSource::Revalidated,
                    Err(TileError::NotModifiedWithoutCachedTile),
                );
            };
            let result = TileData::from_validated_bytes(
                cached.bytes_arc(),
                cached.format,
                cached.content_type.clone(),
                metadata,
            )
            .map(Arc::new);
            (TileSource::Revalidated, result)
        }
        Err(error) => (TileSource::Network, Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        mpsc,
    };

    use super::*;
    use crate::{TileMetadata, UrlTemplateProvider};

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode test PNG");
        bytes.into_inner()
    }

    #[derive(Clone)]
    struct FakeFetcher;

    impl TileFetcher<UrlTemplateProvider> for FakeFetcher {
        fn fetch(
            &self,
            _provider: &UrlTemplateProvider,
            _tile: TileCoordinate,
        ) -> Result<TileData, TileError> {
            TileData::from_bytes(png(), Some("image/png".into()), TileMetadata::default())
        }
    }

    #[derive(Clone)]
    struct PriorityFetcher {
        first_started: mpsc::Sender<()>,
        release_first: Arc<Barrier>,
        calls: Arc<AtomicUsize>,
    }

    impl TileFetcher<UrlTemplateProvider> for PriorityFetcher {
        fn fetch(
            &self,
            _provider: &UrlTemplateProvider,
            _tile: TileCoordinate,
        ) -> Result<TileData, TileError> {
            if self.calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                let _ = self.first_started.send(());
                self.release_first.wait();
            }
            TileData::from_bytes(png(), Some("image/png".into()), TileMetadata::default())
        }
    }

    #[test]
    fn visible_priority_is_served_before_prefetch() {
        let provider =
            UrlTemplateProvider::new("https://example/{z}/{x}/{y}.png", "Example", 19, "test");
        let (first_started, first_started_rx) = mpsc::channel();
        let release_first = Arc::new(Barrier::new(2));
        let scheduler = TileScheduler::new(
            provider,
            PriorityFetcher {
                first_started,
                release_first: Arc::clone(&release_first),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            8,
            None,
            1,
        );
        let first_prefetch = TileCoordinate::new(0, 0, 2);
        let second_prefetch = TileCoordinate::new(2, 0, 2);
        let visible = TileCoordinate::new(1, 0, 2);
        assert!(scheduler.request(first_prefetch, TilePriority::PREFETCH));
        first_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started first fetch");
        assert!(scheduler.request(second_prefetch, TilePriority::PREFETCH));
        assert!(scheduler.request(visible, TilePriority::VISIBLE));
        release_first.wait();

        let first_result = scheduler
            .recv_timeout(Duration::from_secs(2))
            .expect("first result");
        assert_eq!(first_result.tile, first_prefetch);
        let second_result = scheduler
            .recv_timeout(Duration::from_secs(2))
            .expect("second result");
        assert_eq!(second_result.tile, visible);
    }

    #[test]
    fn duplicate_requests_are_coalesced_and_priority_can_be_upgraded() {
        let provider =
            UrlTemplateProvider::new("https://example/{z}/{x}/{y}.png", "Example", 19, "test");
        let scheduler = TileScheduler::new_with_queue_limit(provider, FakeFetcher, 8, None, 1, 8);
        let tile = TileCoordinate::new(0, 0, 2);
        assert!(scheduler.request(tile, TilePriority::PREFETCH));
        assert!(!scheduler.request(tile, TilePriority::PREFETCH));
        assert!(scheduler.request(tile, TilePriority::VISIBLE));
        assert!(scheduler.recv_timeout(Duration::from_secs(2)).is_some());
    }

    #[test]
    fn new_viewport_discards_queued_old_generation() {
        let provider =
            UrlTemplateProvider::new("https://example/{z}/{x}/{y}.png", "Example", 19, "test");
        let (first_started, first_started_rx) = mpsc::channel();
        let release_first = Arc::new(Barrier::new(2));
        let scheduler = TileScheduler::new(
            provider,
            PriorityFetcher {
                first_started,
                release_first: Arc::clone(&release_first),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            8,
            None,
            1,
        );
        let old_generation = scheduler.begin_viewport();
        let executing = TileCoordinate::new(0, 0, 2);
        let obsolete = TileCoordinate::new(1, 0, 2);
        assert!(scheduler.request_for_generation(old_generation, executing, TilePriority::VISIBLE));
        first_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started first fetch");
        assert!(scheduler.request_for_generation(old_generation, obsolete, TilePriority::VISIBLE));

        let new_generation = scheduler.begin_viewport();
        let current = TileCoordinate::new(2, 0, 2);
        assert!(scheduler.request_for_generation(new_generation, current, TilePriority::VISIBLE));
        assert_eq!(scheduler.pending_count(), 2);
        release_first.wait();

        let mut results = Vec::new();
        for _ in 0..2 {
            results.push(
                scheduler
                    .recv_timeout(Duration::from_secs(2))
                    .expect("result from executing and current request"),
            );
        }
        assert!(
            results
                .iter()
                .any(|result| { result.tile == current && result.generation == new_generation })
        );
        assert!(!results.iter().any(|result| result.tile == obsolete));
    }

    #[test]
    fn requests_above_provider_zoom_are_rejected() {
        let provider =
            UrlTemplateProvider::new("https://example/{z}/{x}/{y}.png", "Example", 2, "test");
        let scheduler = TileScheduler::new(provider, FakeFetcher, 8, None, 1);
        assert!(!scheduler.request(TileCoordinate::new(0, 0, 3), TilePriority::VISIBLE));
        assert_eq!(scheduler.pending_count(), 0);
    }
}
