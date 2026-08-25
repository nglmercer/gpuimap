use map_core::TileCoordinate;
use sha2::{Digest, Sha256};

/// Describes how an application obtains and attributes raster tiles.
pub trait TileProvider: Send + Sync + 'static {
    fn tile_url(&self, tile: TileCoordinate) -> String;
    fn attribution(&self) -> &str;
    fn max_zoom(&self) -> u8;
    /// Stable, provider-specific persistent-cache identity.
    fn cache_namespace(&self) -> &str;
}

fn stable_namespace(prefix: &str, identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    format!("{prefix}-{encoded}")
}

/// OpenStreetMap's standard raster provider, useful for development.
///
/// Production deployments should configure a provider whose usage policy and
/// capacity match the application. The endpoint is configurable here so the
/// application does not scatter a public service URL through its UI.
#[derive(Clone, Debug)]
pub struct OpenStreetMapProvider {
    base_url: String,
    namespace: String,
}

impl OpenStreetMapProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        Self {
            namespace: stable_namespace("osm", &base_url),
            base_url,
        }
    }
}

impl Default for OpenStreetMapProvider {
    fn default() -> Self {
        Self::new("https://tile.openstreetmap.org")
    }
}

impl TileProvider for OpenStreetMapProvider {
    fn tile_url(&self, tile: TileCoordinate) -> String {
        format!("{}/{}/{}/{}.png", self.base_url, tile.zoom, tile.x, tile.y)
    }

    fn attribution(&self) -> &str {
        "© OpenStreetMap contributors"
    }

    fn max_zoom(&self) -> u8 {
        19
    }

    fn cache_namespace(&self) -> &str {
        &self.namespace
    }
}

/// A configurable `{z}`, `{x}`, `{y}` URL-template provider.
#[derive(Clone, Debug)]
pub struct UrlTemplateProvider {
    template: String,
    attribution: String,
    max_zoom: u8,
    namespace: String,
}

impl UrlTemplateProvider {
    pub fn new(
        template: impl Into<String>,
        attribution: impl Into<String>,
        max_zoom: u8,
        namespace: impl Into<String>,
    ) -> Self {
        let template = template.into();
        let attribution = attribution.into();
        let namespace = namespace.into();
        let identity = format!("{namespace}\0{template}\0{attribution}\0{max_zoom}");
        Self {
            namespace: stable_namespace("url", &identity),
            template,
            attribution,
            max_zoom,
        }
    }
}

impl TileProvider for UrlTemplateProvider {
    fn tile_url(&self, tile: TileCoordinate) -> String {
        self.template
            .replace("{z}", &tile.zoom.to_string())
            .replace("{x}", &tile.x.to_string())
            .replace("{y}", &tile.y.to_string())
    }

    fn attribution(&self) -> &str {
        &self.attribution
    }

    fn max_zoom(&self) -> u8 {
        self.max_zoom
    }

    fn cache_namespace(&self) -> &str {
        &self.namespace
    }
}

/// A provider for local `/{z}/{x}/{y}.png` tile trees. It is a provider
/// abstraction only; callers choose a file reader instead of sending this URL
/// to the HTTP client.
#[derive(Clone, Debug)]
pub struct LocalTileProvider {
    root: std::path::PathBuf,
    attribution: String,
    max_zoom: u8,
    namespace: String,
}

impl LocalTileProvider {
    pub fn new(root: impl Into<std::path::PathBuf>, attribution: impl Into<String>) -> Self {
        let root = root.into();
        let attribution = attribution.into();
        let identity = format!("{}\0{attribution}", root.to_string_lossy());
        Self {
            namespace: stable_namespace("local", &identity),
            root,
            attribution,
            max_zoom: 19,
        }
    }

    pub fn path_for(&self, tile: TileCoordinate) -> std::path::PathBuf {
        self.root
            .join(tile.zoom.to_string())
            .join(tile.x.to_string())
            .join(format!("{}.png", tile.y))
    }
}

impl TileProvider for LocalTileProvider {
    fn tile_url(&self, tile: TileCoordinate) -> String {
        format!("file://{}", self.path_for(tile).to_string_lossy())
    }

    fn attribution(&self) -> &str {
        &self.attribution
    }

    fn max_zoom(&self) -> u8 {
        self.max_zoom
    }

    fn cache_namespace(&self) -> &str {
        &self.namespace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osm_uses_standard_xyz_path() {
        let provider = OpenStreetMapProvider::default();
        assert_eq!(
            provider.tile_url(TileCoordinate::new(1204, 1534, 12)),
            "https://tile.openstreetmap.org/12/1204/1534.png"
        );
        assert_eq!(provider.max_zoom(), 19);
        assert_ne!(
            provider.cache_namespace(),
            OpenStreetMapProvider::new("https://tiles.example.test").cache_namespace()
        );
    }

    #[test]
    fn template_provider_replaces_all_coordinates() {
        let provider = UrlTemplateProvider::new(
            "https://example.test/{z}/{x}/{y}.jpg",
            "Example",
            18,
            "example",
        );
        assert_eq!(
            provider.tile_url(TileCoordinate::new(4, 5, 6)),
            "https://example.test/6/4/5.jpg"
        );
    }

    #[test]
    fn equivalent_provider_configuration_has_stable_namespace() {
        let first = UrlTemplateProvider::new(
            "https://example.test/{z}/{x}/{y}.jpg",
            "Example",
            18,
            "example",
        );
        let second = UrlTemplateProvider::new(
            "https://example.test/{z}/{x}/{y}.jpg",
            "Example",
            18,
            "example",
        );
        assert_eq!(first.cache_namespace(), second.cache_namespace());
    }
}
