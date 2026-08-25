use std::fmt;

use crate::{GeoPoint, MapCamera, ScreenPoint, TileCoordinate, geo_to_world, world_to_geo};

/// A map viewport in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    pub fn new(width: f32, height: f32) -> Result<Self, ViewportError> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(ViewportError::InvalidSize { width, height });
        }
        Ok(Self { width, height })
    }

    pub fn center(self) -> ScreenPoint {
        ScreenPoint {
            x: self.width / 2.0,
            y: self.height / 2.0,
        }
    }

    /// Converts geographical coordinates to a screen point, choosing the
    /// nearest horizontal copy around the camera at the antimeridian.
    pub fn geo_to_screen(self, camera: &MapCamera, point: GeoPoint) -> ScreenPoint {
        let world_size = crate::world_size(camera.zoom);
        let center = geo_to_world(camera.center, camera.zoom);
        let world = geo_to_world(point, camera.zoom);
        let delta_x = wrapped_world_delta(world.x - center.x, world_size);

        ScreenPoint {
            x: self.width / 2.0 + delta_x as f32,
            y: self.height / 2.0 + (world.y - center.y) as f32,
        }
    }

    /// Converts a screen point to geographical coordinates.
    pub fn screen_to_geo(self, camera: &MapCamera, point: ScreenPoint) -> GeoPoint {
        let center = geo_to_world(camera.center, camera.zoom);
        let world = crate::WorldPoint {
            x: center.x + f64::from(point.x - self.width / 2.0),
            y: center.y + f64::from(point.y - self.height / 2.0),
        };
        world_to_geo(world, camera.zoom)
    }

    /// Calculates the integer XYZ tiles intersecting this viewport.
    pub fn visible_tiles(self, camera: &MapCamera) -> Vec<TileCoordinate> {
        let tile_zoom = camera.zoom.floor().clamp(0.0, 31.0) as u8;
        let tile_count = TileCoordinate::tile_count(tile_zoom).unwrap_or(1);
        let center_world = geo_to_world(camera.center, camera.zoom);
        let current_world_size = crate::world_size(camera.zoom);
        let center_x = center_world.x / current_world_size;
        let center_y = center_world.y / current_world_size;
        let half_width = f64::from(self.width) / (2.0 * current_world_size);
        let half_height = f64::from(self.height) / (2.0 * current_world_size);

        let first_x = ((center_x - half_width) * f64::from(tile_count)).floor() as i64;
        let last_x = ((center_x + half_width) * f64::from(tile_count)).floor() as i64;
        let first_y = ((center_y - half_height) * f64::from(tile_count)).floor() as i64;
        let last_y = ((center_y + half_height) * f64::from(tile_count)).floor() as i64;

        let mut tiles = Vec::new();
        for y in first_y..=last_y {
            for x in first_x..=last_x {
                if let Some(tile) = TileCoordinate::from_wrapped(x, y, tile_zoom)
                    && !tiles.contains(&tile)
                {
                    tiles.push(tile);
                }
            }
        }
        tiles
    }

    /// Calculates where a tile should be placed at the current camera zoom.
    pub fn tile_placement(self, camera: &MapCamera, tile: TileCoordinate) -> TilePlacement {
        let tile_count = f64::from(TileCoordinate::tile_count(tile.zoom).unwrap_or(1));
        let center_world = geo_to_world(camera.center, camera.zoom);
        let current_world_size = crate::world_size(camera.zoom);
        let center_x = center_world.x / current_world_size;
        let center_y = center_world.y / current_world_size;
        let tile_x = f64::from(tile.x) / tile_count;
        let tile_y = f64::from(tile.y) / tile_count;
        let tile_size = current_world_size / tile_count;

        TilePlacement {
            tile,
            origin: ScreenPoint {
                x: (f64::from(self.width) / 2.0
                    + wrapped_normalized_delta(tile_x - center_x) * current_world_size)
                    as f32,
                y: (f64::from(self.height) / 2.0 + (tile_y - center_y) * current_world_size) as f32,
            },
            size: tile_size as f32,
        }
    }
}

/// The screen-space rectangle for one visible tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TilePlacement {
    pub tile: TileCoordinate,
    pub origin: ScreenPoint,
    pub size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewportError {
    InvalidSize { width: f32, height: f32 },
}

impl fmt::Display for ViewportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize { width, height } => {
                write!(
                    formatter,
                    "viewport size must be positive and finite: {width}x{height}"
                )
            }
        }
    }
}

impl std::error::Error for ViewportError {}

fn wrapped_world_delta(delta: f64, world_size: f64) -> f64 {
    (delta + world_size / 2.0).rem_euclid(world_size) - world_size / 2.0
}

fn wrapped_normalized_delta(delta: f64) -> f64 {
    (delta + 0.5).rem_euclid(1.0) - 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MAX_LATITUDE, MIN_LATITUDE, MapCamera};

    fn viewport() -> Viewport {
        Viewport::new(800.0, 600.0).expect("valid viewport")
    }

    #[test]
    fn rejects_invalid_dimensions() {
        assert!(Viewport::new(0.0, 600.0).is_err());
        assert!(Viewport::new(f32::NAN, 600.0).is_err());
    }

    #[test]
    fn center_maps_to_center() {
        let camera = MapCamera::new(GeoPoint::new(-12.0, -77.0), 12.0);
        let screen = viewport().geo_to_screen(&camera, camera.center);
        assert_eq!(screen, viewport().center());
    }

    #[test]
    fn screen_geo_round_trip_handles_antimeridian() {
        let camera = MapCamera::new(GeoPoint::new(0.0, 179.8), 8.0);
        let point = GeoPoint::new(0.2, -179.7);
        let screen = viewport().geo_to_screen(&camera, point);
        let result = viewport().screen_to_geo(&camera, screen);
        assert!((result.latitude - point.latitude).abs() < 1e-5);
        assert!((result.longitude - point.longitude).abs() < 1e-5);
    }

    #[test]
    fn visible_tiles_are_deterministic_and_valid() {
        let camera = MapCamera::new(GeoPoint::new(0.0, 0.0), 2.0);
        let tiles = viewport().visible_tiles(&camera);
        assert_eq!(tiles.len(), 16);
        assert!(tiles.iter().all(|tile| tile.is_valid()));
        assert_eq!(tiles, viewport().visible_tiles(&camera));
    }

    #[test]
    fn visible_tiles_clamp_polar_rows() {
        let camera = MapCamera::new(GeoPoint::new(MAX_LATITUDE, 0.0), 5.0);
        let tiles = viewport().visible_tiles(&camera);
        assert!(tiles.iter().all(|tile| tile.y < 32));
        assert!(tiles.iter().any(|tile| tile.y == 0));

        let camera = MapCamera::new(GeoPoint::new(MIN_LATITUDE, 0.0), 5.0);
        assert!(
            viewport()
                .visible_tiles(&camera)
                .iter()
                .any(|tile| tile.y == 31)
        );
    }

    #[test]
    fn placement_at_camera_center_is_centered() {
        let camera = MapCamera::new(GeoPoint::new(0.0, 0.0), 2.0);
        let placement = viewport().tile_placement(&camera, TileCoordinate::new(1, 1, 2));
        assert_eq!(placement.origin, ScreenPoint { x: 144.0, y: 44.0 });
        assert_eq!(placement.size, 256.0);
    }
}
