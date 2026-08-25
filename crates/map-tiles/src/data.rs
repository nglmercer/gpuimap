use std::{fmt, sync::Arc, time::SystemTime};

/// HTTP/cache metadata retained alongside a tile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileMetadata {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub expires: Option<String>,
    pub cache_control: Option<String>,
    pub downloaded_at: Option<SystemTime>,
}

/// Encoded raster image bytes for one tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileData {
    bytes: Arc<[u8]>,
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
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(TileError::InvalidImage("tile response was empty".into()));
        }
        image::guess_format(&bytes).map_err(|error| TileError::InvalidImage(error.to_string()))?;

        Ok(Self {
            bytes: bytes.into(),
            content_type,
            metadata,
        })
    }

    /// Constructs a tile from already validated bytes. This is used by cache
    /// layers after they have read the bytes from disk and is also useful for
    /// deterministic test fetchers.
    pub fn from_validated_bytes(
        bytes: impl Into<Arc<[u8]>>,
        content_type: Option<String>,
        metadata: TileMetadata,
    ) -> Result<Self, TileError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(TileError::InvalidImage("tile response was empty".into()));
        }
        Ok(Self {
            bytes,
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
    Http(reqwest::Error),
    Io(std::io::Error),
    Cache(String),
    SchedulerClosed,
}

impl fmt::Display for TileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCoordinate => formatter.write_str("invalid XYZ tile coordinate"),
            Self::InvalidImage(error) => write!(formatter, "invalid tile image: {error}"),
            Self::Http(error) => write!(formatter, "tile HTTP request failed: {error}"),
            Self::Io(error) => write!(formatter, "tile cache I/O failed: {error}"),
            Self::Cache(error) => write!(formatter, "tile cache failed: {error}"),
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
