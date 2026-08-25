//! Pure map-domain types and Web Mercator calculations.
//!
//! This crate intentionally has no platform, UI, network, or filesystem
//! dependencies. It can be used from tests, a renderer, or a future mobile
//!/server front end without changing the map math.

mod camera;
mod coordinate;
mod mercator;
mod viewport;

pub use camera::{DEFAULT_ZOOM, MAX_ZOOM, MIN_ZOOM, MapCamera};
pub use coordinate::{
    CoordinateError, GeoPoint, MAX_LATITUDE, MIN_LATITUDE, ScreenPoint, TileCoordinate, WorldPoint,
};
pub use mercator::{TILE_SIZE, geo_to_tile, geo_to_world, tile_to_world, world_size, world_to_geo};
pub use viewport::{TilePlacement, Viewport, ViewportError};
