use std::{fs, io::Read, time::SystemTime};

use map_core::TileCoordinate;

use crate::{
    LocalTileProvider, TileData, TileError, TileMetadata, TileProvider, TileValidationLimits,
};

/// The outcome of a provider request. A conditional request can refresh
/// metadata without downloading another copy of the encoded tile.
#[derive(Debug)]
pub enum TileFetchResult {
    Updated(TileData),
    NotModified(TileMetadata),
}

/// A fetcher used by the scheduler. It is a trait so tests and local tile
/// sources can run without a network server.
pub trait TileFetcher<P: TileProvider>: Send + Sync + 'static {
    fn fetch(&self, provider: &P, tile: TileCoordinate) -> Result<TileData, TileError>;

    fn fetch_with_cache(
        &self,
        provider: &P,
        tile: TileCoordinate,
        _cached: Option<&TileData>,
    ) -> Result<TileFetchResult, TileError> {
        self.fetch(provider, tile).map(TileFetchResult::Updated)
    }
}

/// Blocking HTTP client intended to run only on tile worker threads.
#[derive(Clone, Debug)]
pub struct HttpTileClient {
    client: reqwest::blocking::Client,
    user_agent: String,
    limits: TileValidationLimits,
}

impl HttpTileClient {
    pub fn new(user_agent: impl Into<String>) -> Result<Self, TileError> {
        Self::with_limits(user_agent, TileValidationLimits::default())
    }

    pub fn with_limits(
        user_agent: impl Into<String>,
        limits: TileValidationLimits,
    ) -> Result<Self, TileError> {
        let user_agent = user_agent.into();
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(user_agent.clone())
            .build()?;
        Ok(Self {
            client,
            user_agent,
            limits,
        })
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub fn limits(&self) -> TileValidationLimits {
        self.limits
    }
}

impl<P: TileProvider> TileFetcher<P> for HttpTileClient {
    fn fetch(&self, provider: &P, tile: TileCoordinate) -> Result<TileData, TileError> {
        match self.fetch_with_cache(provider, tile, None)? {
            TileFetchResult::Updated(data) => Ok(data),
            TileFetchResult::NotModified(_) => Err(TileError::NotModifiedWithoutCachedTile),
        }
    }

    fn fetch_with_cache(
        &self,
        provider: &P,
        tile: TileCoordinate,
        cached: Option<&TileData>,
    ) -> Result<TileFetchResult, TileError> {
        validate_tile(provider, tile)?;

        let mut request = self.client.get(provider.tile_url(tile));
        if let Some(cached) = cached {
            if let Some(etag) = &cached.metadata.etag {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = &cached.metadata.last_modified {
                request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
            }
        }

        let response = request.send()?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            let headers = response.headers();
            let metadata = metadata_from_headers(headers, Some(SystemTime::now()));
            let Some(cached) = cached else {
                return Err(TileError::NotModifiedWithoutCachedTile);
            };
            return Ok(TileFetchResult::NotModified(
                cached.metadata.merge_revalidation(metadata),
            ));
        }

        let response = response.error_for_status()?;
        let headers = response.headers();
        let metadata = metadata_from_headers(headers, Some(SystemTime::now()));
        let content_type = header_value(headers, reqwest::header::CONTENT_TYPE);
        let bytes = read_bounded(response, self.limits.max_encoded_bytes)?;
        TileData::from_bytes_with_limits(bytes, content_type, metadata, self.limits)
            .map(TileFetchResult::Updated)
    }
}

fn read_bounded(
    response: reqwest::blocking::Response,
    maximum: usize,
) -> Result<Vec<u8>, TileError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(TileError::EncodedSizeExceeded {
            actual: response.content_length().unwrap_or(u64::MAX) as usize,
            maximum,
        });
    }

    let capacity = response
        .content_length()
        .map_or(0, |length| (length as usize).min(maximum));
    let mut bytes = Vec::with_capacity(capacity);
    response
        .take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(TileError::EncodedSizeExceeded {
            actual: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
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
        .and_then(|format| match format {
            image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP => {
                Some(format.to_mime_type().to_owned())
            }
            _ => None,
        })
}

fn metadata_from_headers(
    headers: &reqwest::header::HeaderMap,
    downloaded_at: Option<SystemTime>,
) -> TileMetadata {
    TileMetadata {
        etag: header_value(headers, reqwest::header::ETAG),
        last_modified: header_value(headers, reqwest::header::LAST_MODIFIED),
        expires: header_value(headers, reqwest::header::EXPIRES),
        cache_control: header_value(headers, reqwest::header::CACHE_CONTROL),
        downloaded_at,
        ..TileMetadata::default()
    }
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
    use crate::UrlTemplateProvider;
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
    };

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
        assert_eq!(data.format, crate::TileFormat::Png);
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

    #[test]
    fn http_fetcher_revalidates_stale_tiles_and_honors_detected_format() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test server");
        let port = listener.local_addr().expect("server address").port();
        let response_body = jpeg();
        let response_body_length = response_body.len();
        let (conditional_header_tx, conditional_header_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept test request");
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                loop {
                    let count = stream.read(&mut buffer).expect("read test request");
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                if request_index == 0 {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nCache-Control: max-age=0\r\nETag: \"v1\"\r\nContent-Length: {response_body_length}\r\nConnection: close\r\n\r\n"
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write initial headers");
                    stream
                        .write_all(&response_body)
                        .expect("write initial body");
                } else {
                    conditional_header_tx
                        .send(
                            request
                                .to_ascii_lowercase()
                                .contains("if-none-match: \"v1\""),
                        )
                        .expect("send conditional-header result");
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nCache-Control: max-age=60\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write revalidation response");
                }
            }
        });

        let provider = UrlTemplateProvider::new(
            format!("http://127.0.0.1:{port}/{{z}}/{{x}}/{{y}}"),
            "Test",
            19,
            "test",
        );
        let tile = TileCoordinate::new(0, 0, 2);
        let client = HttpTileClient::new("gpuimap-test/0.1").expect("HTTP client");
        let first = match client
            .fetch_with_cache(&provider, tile, None)
            .expect("initial HTTP response")
        {
            TileFetchResult::Updated(data) => data,
            TileFetchResult::NotModified(_) => panic!("initial response was not a tile"),
        };
        assert_eq!(first.format, crate::TileFormat::Jpeg);
        assert_eq!(first.content_type.as_deref(), Some("image/png"));

        let revalidated = match client
            .fetch_with_cache(&provider, tile, Some(&first))
            .expect("conditional HTTP response")
        {
            TileFetchResult::NotModified(metadata) => metadata,
            TileFetchResult::Updated(_) => panic!("server should have returned 304"),
        };
        assert!(revalidated.is_fresh(SystemTime::now()));
        assert!(
            conditional_header_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("conditional request observed")
        );
        server.join().expect("test server");
    }

    fn jpeg() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .expect("encode test JPEG");
        bytes.into_inner()
    }
}
