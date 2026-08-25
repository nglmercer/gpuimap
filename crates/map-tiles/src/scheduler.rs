use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

use map_core::TileCoordinate;

use crate::{
    DiskTileCache, HttpTileClient, MemoryTileCache, TileData, TileError, TileFetcher, TileProvider,
};

/// Priority values used by the scheduler. Higher values run first.
pub struct TilePriority;

impl TilePriority {
    pub const PREFETCH: i32 = 10;
    pub const NEARBY: i32 = 50;
    pub const VISIBLE: i32 = 100;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileSource {
    Memory,
    Disk,
    Network,
}

pub struct TileResult {
    pub tile: TileCoordinate,
    pub source: TileSource,
    pub result: Result<Arc<TileData>, TileError>,
}

#[derive(Clone, Debug)]
struct QueuedRequest {
    tile: TileCoordinate,
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
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for QueuedRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct QueueState {
    queue: BinaryHeap<QueuedRequest>,
    pending: HashSet<TileCoordinate>,
    next_sequence: u64,
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

/// Background tile scheduler with bounded worker concurrency.
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
        let (results_tx, results_rx) = mpsc::channel();
        let inner = Arc::new(SchedulerInner {
            provider: Arc::new(provider),
            fetcher: Arc::new(fetcher),
            memory: MemoryTileCache::new(memory_capacity),
            disk,
            queue: Mutex::new(QueueState {
                queue: BinaryHeap::new(),
                pending: HashSet::new(),
                next_sequence: 0,
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

    pub fn request(&self, tile: TileCoordinate, priority: i32) -> bool {
        if !self.workers_available || !tile.is_valid() || tile.zoom > self.inner.provider.max_zoom()
        {
            return false;
        }

        if let Some(data) = self
            .inner
            .memory
            .get(self.inner.provider.cache_namespace(), tile)
        {
            let _ = self.inner.results_tx.send(TileResult {
                tile,
                source: TileSource::Memory,
                result: Ok(data),
            });
            return true;
        }

        let Ok(mut state) = self.inner.queue.lock() else {
            return false;
        };
        if state.shutting_down || !state.pending.insert(tile) {
            return false;
        }
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        state.queue.push(QueuedRequest {
            tile,
            priority,
            sequence,
        });
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
            .map(|state| state.pending.len())
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
        }
        self.inner.wake_workers.notify_all();
        for handle in worker_handles {
            let _ = handle.join();
        }
    }
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
            while state.queue.is_empty() && !state.shutting_down {
                let Ok(next_state) = inner.wake_workers.wait(state) else {
                    return;
                };
                state = next_state;
            }
            if state.shutting_down && state.queue.is_empty() {
                return;
            }
            state.queue.pop()
        };

        let Some(request) = request else {
            continue;
        };
        let namespace = inner.provider.cache_namespace();
        let (source, result) = if let Some(data) = inner.memory.get(namespace, request.tile) {
            (TileSource::Memory, Ok(data))
        } else if let Some(disk) = &inner.disk {
            match disk.get(request.tile) {
                Ok(Some(data)) => {
                    let data = Arc::new(data);
                    inner
                        .memory
                        .insert(namespace, request.tile, Arc::clone(&data));
                    (TileSource::Disk, Ok(data))
                }
                Ok(None) => fetch_from_network(&inner, request.tile),
                // A damaged, read-only, or otherwise inaccessible local
                // cache must not prevent a fresh network tile from loading.
                Err(_) => fetch_from_network(&inner, request.tile),
            }
        } else {
            fetch_from_network(&inner, request.tile)
        };

        if let Ok(data) = &result
            && source == TileSource::Network
        {
            inner
                .memory
                .insert(namespace, request.tile, Arc::clone(data));
            if let Some(disk) = &inner.disk {
                // A full or read-only cache must not make a successfully
                // downloaded tile disappear from the UI.
                let _ = disk.insert(request.tile, data);
            }
        }

        if let Ok(mut state) = inner.queue.lock() {
            state.pending.remove(&request.tile);
        }
        let _ = inner.results_tx.send(TileResult {
            tile: request.tile,
            source,
            result,
        });
    }
}

fn fetch_from_network<P, F>(
    inner: &SchedulerInner<P, F>,
    tile: TileCoordinate,
) -> (TileSource, Result<Arc<TileData>, TileError>)
where
    P: TileProvider,
    F: TileFetcher<P>,
{
    match inner.fetcher.fetch(&inner.provider, tile) {
        Ok(data) => (TileSource::Network, Ok(Arc::new(data))),
        Err(error) => (TileSource::Network, Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
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
    fn duplicate_requests_are_coalesced() {
        let provider =
            UrlTemplateProvider::new("https://example/{z}/{x}/{y}.png", "Example", 19, "test");
        let scheduler = TileScheduler::new(provider, FakeFetcher, 8, None, 1);
        let tile = TileCoordinate::new(0, 0, 2);
        assert!(scheduler.request(tile, TilePriority::VISIBLE));
        assert!(!scheduler.request(tile, TilePriority::VISIBLE));
        assert!(scheduler.recv_timeout(Duration::from_secs(2)).is_some());
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
