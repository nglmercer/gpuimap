use std::{fs, time::SystemTime};

use map_core::TileCoordinate;

use crate::{LocalTileProvider, TileData, TileError, TileMetadata, TileProvider};

/// A fetcher used by the scheduler. It is a trait so tests and local tile
/// sources can run without a network server.
pub trait TileFetcher<P: TileProvider>: Send + Sync + 'static {
    fn fetch(&self, provider: &P, tile: TileCoordinate) -> Result<TileData, TileError>;
}

/// Blocking HTTP client intended to run only on tile worker threads.
#[derive(Clone, Debug)]
pub struct HttpTileClient {
    client: reqwest::blocking::Client,
    user_agent: String,
}

impl HttpTileClient {
    pub fn new(user_agent: impl Into<String>) -> Result<Self, TileError> {
        let user_agent = user_agent.into();
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(user_agent.clone())
            .build()?;
        Ok(Self { client, user_agent })
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

impl<P: TileProvider> TileFetcher<P> for HttpTileClient {
    fn fetch(&self, provider: &P, tile: TileCoordinate) -> Result<TileData, TileError> {
        validate_tile(provider, tile)?;

        let response = self
            .client
            .get(provider.tile_url(tile))
            .send()?
            .error_for_status()?;
        let headers = response.headers();
        let metadata = TileMetadata {
            etag: header_value(headers, reqwest::header::ETAG),
            last_modified: header_value(headers, reqwest::header::LAST_MODIFIED),
            expires: header_value(headers, reqwest::header::EXPIRES),
            cache_control: header_value(headers, reqwest::header::CACHE_CONTROL),
            downloaded_at: Some(SystemTime::now()),
        };
        let content_type = header_value(headers, reqwest::header::CONTENT_TYPE);
        let bytes = response.bytes()?.to_vec();
        TileData::from_bytes(bytes, content_type, metadata)
    }
}

/// File-backed fetcher for a local `/{z}/{x}/{y}.png` tile tree.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalTileFetcher;

impl LocalTileFetcher {
    pub const fn new() -> Self {
        Self
    }
}

impl TileFetcher<LocalTileProvider> for LocalTileFetcher {
    fn fetch(
        &self,
        provider: &LocalTileProvider,
        tile: TileCoordinate,
    ) -> Result<TileData, TileError> {
        validate_tile(provider, tile)?;
        let bytes = fs::read(provider.path_for(tile))?;
        let content_type = detected_content_type(&bytes);
        TileData::from_bytes(
            bytes,
            content_type,
            TileMetadata {
                downloaded_at: Some(SystemTime::now()),
                ..TileMetadata::default()
            },
        )
    }
}

fn validate_tile<P: TileProvider>(provider: &P, tile: TileCoordinate) -> Result<(), TileError> {
    if !tile.is_valid() || tile.zoom > provider.max_zoom() {
        Err(TileError::InvalidCoordinate)
    } else {
        Ok(())
    }
}

fn detected_content_type(bytes: &[u8]) -> Option<String> {
    image::guess_format(bytes)
        .ok()
        .map(|format| format.to_mime_type().to_owned())
}

fn header_value(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode test PNG");
        bytes.into_inner()
    }

    #[test]
    fn local_fetcher_reads_xyz_file_and_detects_mime_type() {
        let root =
            std::env::temp_dir().join(format!("gpuimap-local-tile-test-{}", std::process::id()));
        let tile = TileCoordinate::new(1, 2, 3);
        let tile_directory = root.join(tile.zoom.to_string()).join(tile.x.to_string());
        let tile_path = tile_directory.join(format!("{}.png", tile.y));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&tile_directory).expect("tile directory");
        fs::write(&tile_path, png()).expect("tile bytes");

        let provider = LocalTileProvider::new(&root, "Local tiles");
        let data = LocalTileFetcher::new()
            .fetch(&provider, tile)
            .expect("local tile");
        assert_eq!(data.content_type.as_deref(), Some("image/png"));
        assert!(!data.bytes().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fetcher_rejects_tiles_above_provider_limit() {
        let provider = LocalTileProvider::new("tiles", "Local tiles");
        let error = LocalTileFetcher::new()
            .fetch(&provider, TileCoordinate::new(0, 0, 20))
            .expect_err("zoom should be rejected");
        assert!(matches!(error, TileError::InvalidCoordinate));
    }
}
