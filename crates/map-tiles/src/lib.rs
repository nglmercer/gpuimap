//! Raster XYZ tile providers, caches, and background scheduling.

mod cache;
mod client;
mod data;
mod provider;
mod scheduler;

pub use cache::{DiskCachePolicy, DiskTileCache, MemoryTileCache};
pub use client::{HttpTileClient, LocalTileFetcher, TileFetchResult, TileFetcher};
pub use data::{TileData, TileError, TileFormat, TileMetadata, TileValidationLimits};
pub use provider::{LocalTileProvider, OpenStreetMapProvider, TileProvider, UrlTemplateProvider};
pub use scheduler::{
    DEFAULT_MAX_QUEUED_REQUESTS, TileGeneration, TilePriority, TileResult, TileScheduler,
    TileService, TileSource,
};
