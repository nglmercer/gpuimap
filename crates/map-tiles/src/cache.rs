use std::{
    collections::{HashMap, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

use map_core::TileCoordinate;
use storage::CachePaths;

use crate::{TileData, TileError, TileMetadata};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TileKey {
    namespace: String,
    tile: TileCoordinate,
}

/// Bounded LRU cache for decoded-or-encoded tile data.
#[derive(Clone, Debug)]
pub struct MemoryTileCache {
    capacity: usize,
    state: Arc<Mutex<MemoryState>>,
}

#[derive(Debug, Default)]
struct MemoryState {
    entries: HashMap<TileKey, Arc<TileData>>,
    order: VecDeque<TileKey>,
}

impl MemoryTileCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Arc::new(Mutex::new(MemoryState::default())),
        }
    }

    pub fn get(&self, namespace: &str, tile: TileCoordinate) -> Option<Arc<TileData>> {
        let key = TileKey {
            namespace: namespace.to_owned(),
            tile,
        };
        let mut state = self.state.lock().ok()?;
        let value = state.entries.get(&key).cloned()?;
        touch(&mut state.order, &key);
        Some(value)
    }

    pub fn insert(&self, namespace: &str, tile: TileCoordinate, data: Arc<TileData>) {
        if self.capacity == 0 {
            return;
        }
        let key = TileKey {
            namespace: namespace.to_owned(),
            tile,
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.entries.insert(key.clone(), data);
        touch(&mut state.order, &key);
        while state.entries.len() > self.capacity {
            let Some(oldest) = state.order.pop_back() else {
                break;
            };
            state.entries.remove(&oldest);
        }
    }

    pub fn len(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.entries.len())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn touch(order: &mut VecDeque<TileKey>, key: &TileKey) {
    if let Some(index) = order.iter().position(|item| item == key) {
        order.remove(index);
    }
    order.push_front(key.clone());
}

/// Persistent cache for one tile-provider namespace.
#[derive(Clone, Debug)]
pub struct DiskTileCache {
    root: PathBuf,
    namespace: String,
}

impl DiskTileCache {
    pub fn new(paths: &CachePaths, namespace: &str) -> Result<Self, TileError> {
        let root = paths
            .tile_namespace(namespace)
            .map_err(|error| TileError::Cache(error.to_string()))?;
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            namespace: namespace.to_owned(),
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn get(&self, tile: TileCoordinate) -> Result<Option<TileData>, TileError> {
        if !tile.is_valid() {
            return Err(TileError::InvalidCoordinate);
        }
        let path = self.tile_path(tile);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = read_metadata(&path.with_extension("meta"))?;
        let content_type = image::guess_format(&bytes)
            .ok()
            .map(|format| format.to_mime_type().to_owned());
        TileData::from_bytes(bytes, content_type, metadata).map(Some)
    }

    pub fn insert(&self, tile: TileCoordinate, data: &TileData) -> Result<(), TileError> {
        if !tile.is_valid() {
            return Err(TileError::InvalidCoordinate);
        }
        let path = self.tile_path(tile);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let nonce = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("png.tmp-{}-{nonce}", std::process::id()));
        let metadata_path = path.with_extension("meta");
        let metadata_temporary =
            metadata_path.with_extension(format!("meta.tmp-{}-{nonce}", std::process::id()));
        fs::write(&temporary, data.bytes())?;
        replace_file(&temporary, &path)?;
        write_metadata(&metadata_temporary, &data.metadata)?;
        replace_file(&metadata_temporary, &metadata_path)?;
        Ok(())
    }

    fn tile_path(&self, tile: TileCoordinate) -> PathBuf {
        self.root
            .join(tile.zoom.to_string())
            .join(tile.x.to_string())
            .join(format!("{}.png", tile.y))
    }
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<(), io::Error> {
    match fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(temporary, destination)
}

fn write_metadata(path: &Path, metadata: &TileMetadata) -> Result<(), TileError> {
    let contents = [
        metadata_line("etag", metadata.etag.as_deref()),
        metadata_line("last_modified", metadata.last_modified.as_deref()),
        metadata_line("expires", metadata.expires.as_deref()),
        metadata_line("cache_control", metadata.cache_control.as_deref()),
        metadata_line(
            "downloaded_at",
            metadata.downloaded_at.and_then(|time| {
                time.duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs().to_string())
            }),
        ),
    ]
    .join("");
    fs::write(path, contents)?;
    Ok(())
}

fn metadata_line(key: &str, value: Option<impl AsRef<str>>) -> String {
    let value = value.map_or_else(String::new, |value| sanitize_metadata(value.as_ref()));
    format!("{key}={value}\n")
}

fn sanitize_metadata(value: &str) -> String {
    value.replace(['\r', '\n', '='], " ")
}

fn read_metadata(path: &Path) -> Result<TileMetadata, TileError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(TileMetadata::default()),
        Err(error) => return Err(error.into()),
    };
    let mut metadata = TileMetadata::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = (!value.is_empty()).then(|| value.to_owned());
        match key {
            "etag" => metadata.etag = value,
            "last_modified" => metadata.last_modified = value,
            "expires" => metadata.expires = value,
            "cache_control" => metadata.cache_control = value,
            "downloaded_at" => {
                metadata.downloaded_at = value
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds));
            }
            _ => {}
        }
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode test PNG");
        bytes.into_inner()
    }

    fn tile(value: u8) -> Arc<TileData> {
        Arc::new(
            TileData::from_bytes(
                png(),
                Some("image/png".into()),
                TileMetadata {
                    etag: Some(value.to_string()),
                    ..Default::default()
                },
            )
            .expect("fixture is a PNG"),
        )
    }

    #[test]
    fn memory_cache_evicts_least_recently_used_entry() {
        let cache = MemoryTileCache::new(2);
        cache.insert("osm", TileCoordinate::new(0, 0, 1), tile(1));
        cache.insert("osm", TileCoordinate::new(1, 0, 1), tile(2));
        assert!(cache.get("osm", TileCoordinate::new(0, 0, 1)).is_some());
        cache.insert("osm", TileCoordinate::new(0, 1, 1), tile(3));
        assert!(cache.get("osm", TileCoordinate::new(1, 0, 1)).is_none());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn disk_cache_round_trips_metadata() {
        let root = std::env::temp_dir().join(format!("gpuimap-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let paths = CachePaths::from_root(&root);
        let cache = DiskTileCache::new(&paths, "test").expect("cache directory");
        let data = TileData::from_bytes(
            png(),
            Some("image/png".into()),
            TileMetadata {
                etag: Some("abc".into()),
                downloaded_at: Some(UNIX_EPOCH + Duration::from_secs(42)),
                ..Default::default()
            },
        )
        .expect("fixture is a PNG");
        let coordinate = TileCoordinate::new(3, 2, 2);
        cache.insert(coordinate, &data).expect("write tile");
        let loaded = cache
            .get(coordinate)
            .expect("read tile")
            .expect("tile exists");
        assert_eq!(loaded.bytes(), png().as_slice());
        assert_eq!(loaded.content_type.as_deref(), Some("image/png"));
        assert_eq!(loaded.metadata.etag.as_deref(), Some("abc"));
        assert_eq!(loaded.metadata.downloaded_at, data.metadata.downloaded_at);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn disk_cache_rejects_coordinates_outside_the_tile_matrix() {
        let root =
            std::env::temp_dir().join(format!("gpuimap-cache-invalid-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let paths = CachePaths::from_root(&root);
        let cache = DiskTileCache::new(&paths, "test").expect("cache directory");
        let invalid = TileCoordinate::new(4, 0, 2);
        assert!(matches!(
            cache.get(invalid),
            Err(TileError::InvalidCoordinate)
        ));
        let data = TileData::from_bytes(png(), Some("image/png".into()), TileMetadata::default())
            .expect("fixture is a PNG");
        assert!(matches!(
            cache.insert(invalid, &data),
            Err(TileError::InvalidCoordinate)
        ));
        let _ = fs::remove_dir_all(root);
    }
}
