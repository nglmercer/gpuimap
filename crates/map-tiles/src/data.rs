use std::{
    fmt,
    io::Cursor,
    sync::Arc,
    time::{Duration, SystemTime},
};

use httpdate::parse_http_date;

/// The image formats accepted by the raster tile pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileFormat {
    Png,
    Jpeg,
    Webp,
}

impl TileFormat {
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    fn from_image_format(format: image::ImageFormat) -> Option<Self> {
        match format {
            image::ImageFormat::Png => Some(Self::Png),
            image::ImageFormat::Jpeg => Some(Self::Jpeg),
            image::ImageFormat::WebP => Some(Self::Webp),
            _ => None,
        }
    }
}

/// Limits applied before tile bytes can reach a renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileValidationLimits {
    pub max_encoded_bytes: usize,
    pub max_dimension: u32,
    pub max_pixels: u64,
}

impl Default for TileValidationLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 8 * 1024 * 1024,
            max_dimension: 4_096,
            max_pixels: 16 * 1024 * 1024,
        }
    }
}

/// HTTP/cache metadata retained alongside a tile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileMetadata {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub expires: Option<String>,
    pub cache_control: Option<String>,
    pub downloaded_at: Option<SystemTime>,
    pub last_accessed_at: Option<SystemTime>,
}

impl TileMetadata {
    /// Returns whether the response may be used without contacting its
    /// provider again. Responses without an explicit freshness lifetime are
    /// treated as stale so they can be revalidated rather than living in the
    /// cache forever.
    pub fn is_fresh(&self, now: SystemTime) -> bool {
        let Some(downloaded_at) = self.downloaded_at else {
            return false;
        };

        let cache_control = self.cache_control.as_deref().unwrap_or_default();
        if has_cache_control_directive(cache_control, "no-cache")
            || has_cache_control_directive(cache_control, "no-store")
        {
            return false;
        }

        if let Some(max_age) = cache_control_max_age(cache_control) {
            return downloaded_at
                .checked_add(Duration::from_secs(max_age))
                .is_some_and(|deadline| now <= deadline);
        }

        self.expires
            .as_deref()
            .and_then(|value| parse_http_date(value).ok())
            .is_some_and(|deadline| now <= deadline)
    }

    /// Returns whether the response is allowed to be persisted.
    pub fn is_storeable(&self) -> bool {
        !has_cache_control_directive(
            self.cache_control.as_deref().unwrap_or_default(),
            "no-store",
        )
    }

    /// Combines a cached response with metadata returned by a conditional
    /// request. A 304 response may omit validators or freshness headers, in
    /// which case the cached values remain authoritative.
    pub fn merge_revalidation(&self, newer: Self) -> Self {
        Self {
            etag: newer.etag.or_else(|| self.etag.clone()),
            last_modified: newer.last_modified.or_else(|| self.last_modified.clone()),
            expires: newer.expires.or_else(|| self.expires.clone()),
            cache_control: newer.cache_control.or_else(|| self.cache_control.clone()),
            downloaded_at: newer.downloaded_at.or(self.downloaded_at),
            last_accessed_at: newer.last_accessed_at.or(self.last_accessed_at),
        }
    }
}

fn has_cache_control_directive(value: &str, expected: &str) -> bool {
    value.split(',').any(|directive| {
        let name = directive
            .trim()
            .split_once('=')
            .map_or(directive.trim(), |(name, _)| name.trim());
        name.eq_ignore_ascii_case(expected)
    })
}

fn cache_control_max_age(value: &str) -> Option<u64> {
    value.split(',').find_map(|directive| {
        let (name, value) = directive.trim().split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("max-age") {
            return None;
        }
        value.trim().trim_matches('"').parse().ok()
    })
}

/// Encoded raster image bytes for one tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileData {
    bytes: Arc<[u8]>,
    /// Detected from the bytes; this is authoritative even when an HTTP
    /// server sends a missing or incorrect Content-Type header.
    pub format: TileFormat,
    pub content_type: Option<String>,
    pub metadata: TileMetadata,
}

impl TileData {
    /// Validates and stores encoded PNG/JPEG/WebP bytes without tying this
    /// crate to a renderer such as GPUI.
    pub fn from_bytes(
        bytes: impl Into<Vec<u8>>,
        content_type: Option<String>,
        metadata: TileMetadata,
    ) -> Result<Self, TileError> {
        Self::from_bytes_with_limits(
            bytes,
            content_type,
            metadata,
            TileValidationLimits::default(),
        )
    }

    /// Validates encoded bytes using caller-supplied resource limits.
    pub fn from_bytes_with_limits(
        bytes: impl Into<Vec<u8>>,
        content_type: Option<String>,
        metadata: TileMetadata,
        limits: TileValidationLimits,
    ) -> Result<Self, TileError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(TileError::InvalidImage("tile response was empty".into()));
        }
        if bytes.len() > limits.max_encoded_bytes {
            return Err(TileError::EncodedSizeExceeded {
                actual: bytes.len(),
                maximum: limits.max_encoded_bytes,
            });
        }

        let detected = image::guess_format(&bytes)
            .map_err(|error| TileError::InvalidImage(error.to_string()))?;
        let format = TileFormat::from_image_format(detected).ok_or_else(|| {
            TileError::UnsupportedFormat(format!("recognized format {detected:?}"))
        })?;
        let reader = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|error| TileError::InvalidImage(error.to_string()))?;
        let (width, height) = reader
            .into_dimensions()
            .map_err(|error| TileError::InvalidImage(error.to_string()))?;
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if width > limits.max_dimension
            || height > limits.max_dimension
            || pixels > limits.max_pixels
        {
            return Err(TileError::DimensionsExceeded {
                width,
                height,
                max_dimension: limits.max_dimension,
                max_pixels: limits.max_pixels,
            });
        }

        Ok(Self {
            bytes: bytes.into(),
            format,
            content_type,
            metadata,
        })
    }

    /// Constructs a tile from bytes that were already validated. This is used
    /// when a 304 response reuses the cached encoded payload.
    pub(crate) fn from_validated_bytes(
        bytes: impl Into<Arc<[u8]>>,
        format: TileFormat,
        content_type: Option<String>,
        metadata: TileMetadata,
    ) -> Result<Self, TileError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(TileError::InvalidImage("tile response was empty".into()));
        }
        Ok(Self {
            bytes,
            format,
            content_type,
            metadata,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn bytes_arc(&self) -> Arc<[u8]> {
        self.bytes.clone()
    }
}

/// All recoverable tile-pipeline failures are represented by this type.
#[derive(Debug)]
pub enum TileError {
    InvalidCoordinate,
    InvalidImage(String),
    UnsupportedFormat(String),
    EncodedSizeExceeded {
        actual: usize,
        maximum: usize,
    },
    DimensionsExceeded {
        width: u32,
        height: u32,
        max_dimension: u32,
        max_pixels: u64,
    },
    Http(reqwest::Error),
    Io(std::io::Error),
    Cache(String),
    NotModifiedWithoutCachedTile,
    SchedulerClosed,
}

impl fmt::Display for TileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCoordinate => formatter.write_str("invalid XYZ tile coordinate"),
            Self::InvalidImage(error) => write!(formatter, "invalid tile image: {error}"),
            Self::UnsupportedFormat(format) => {
                write!(formatter, "unsupported tile image format: {format}")
            }
            Self::EncodedSizeExceeded { actual, maximum } => write!(
                formatter,
                "tile response is {actual} bytes, exceeding the {maximum}-byte limit"
            ),
            Self::DimensionsExceeded {
                width,
                height,
                max_dimension,
                max_pixels,
            } => write!(
                formatter,
                "tile dimensions {width}x{height} exceed {max_dimension}px/{max_pixels} pixels"
            ),
            Self::Http(error) => write!(formatter, "tile HTTP request failed: {error}"),
            Self::Io(error) => write!(formatter, "tile cache I/O failed: {error}"),
            Self::Cache(error) => write!(formatter, "tile cache failed: {error}"),
            Self::NotModifiedWithoutCachedTile => {
                formatter.write_str("tile server returned 304 without a cached tile")
            }
            Self::SchedulerClosed => formatter.write_str("tile scheduler is closed"),
        }
    }
}

impl std::error::Error for TileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for TileError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

impl From<std::io::Error> for TileError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode test PNG");
        bytes.into_inner()
    }

    #[test]
    fn format_is_detected_from_bytes_not_content_type() {
        let data = TileData::from_bytes(png(), Some("image/jpeg".into()), TileMetadata::default())
            .expect("fixture is a PNG");
        assert_eq!(data.format, TileFormat::Png);
    }

    #[test]
    fn encoded_size_limit_is_enforced() {
        let error = TileData::from_bytes_with_limits(
            png(),
            Some("image/png".into()),
            TileMetadata::default(),
            TileValidationLimits {
                max_encoded_bytes: 1,
                ..TileValidationLimits::default()
            },
        )
        .expect_err("fixture should exceed the limit");
        assert!(matches!(error, TileError::EncodedSizeExceeded { .. }));
    }

    #[test]
    fn dimensions_limit_is_enforced_before_rendering() {
        let error = TileData::from_bytes_with_limits(
            png(),
            Some("image/png".into()),
            TileMetadata::default(),
            TileValidationLimits {
                max_dimension: 0,
                max_pixels: 0,
                ..TileValidationLimits::default()
            },
        )
        .expect_err("fixture should exceed the dimensions limit");
        assert!(matches!(error, TileError::DimensionsExceeded { .. }));
    }

    #[test]
    fn cache_control_max_age_controls_freshness() {
        let downloaded_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let metadata = TileMetadata {
            cache_control: Some("public, max-age=60".into()),
            downloaded_at: Some(downloaded_at),
            ..TileMetadata::default()
        };
        assert!(metadata.is_fresh(downloaded_at + Duration::from_secs(60)));
        assert!(!metadata.is_fresh(downloaded_at + Duration::from_secs(61)));
    }

    #[test]
    fn explicit_expiration_is_used_when_max_age_is_absent() {
        let downloaded_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let expires = httpdate::fmt_http_date(downloaded_at + Duration::from_secs(60));
        let metadata = TileMetadata {
            expires: Some(expires),
            downloaded_at: Some(downloaded_at),
            ..TileMetadata::default()
        };
        assert!(metadata.is_fresh(downloaded_at + Duration::from_secs(60)));
        assert!(!metadata.is_fresh(downloaded_at + Duration::from_secs(61)));
    }
}
