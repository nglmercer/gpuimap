use crate::{GeoPoint, MAX_LATITUDE, MIN_LATITUDE, TileCoordinate, WorldPoint};

/// The nominal size of a raster tile in pixels.
pub const TILE_SIZE: f64 = 256.0;

/// Returns the projected world width/height in pixels for a zoom level.
pub fn world_size(zoom: f64) -> f64 {
    TILE_SIZE * 2.0_f64.powf(zoom)
}

/// Converts a geographical point to Web Mercator world pixels.
pub fn geo_to_world(point: GeoPoint, zoom: f64) -> WorldPoint {
    let latitude = point.latitude.clamp(MIN_LATITUDE, MAX_LATITUDE);
    let longitude = point.longitude;
    let x = (longitude + 180.0) / 360.0;
    let latitude_radians = latitude.to_radians();
    let mercator_y = (1.0
        - (latitude_radians.tan() + 1.0 / latitude_radians.cos()).ln() / std::f64::consts::PI)
        / 2.0;
    let size = world_size(zoom);

    WorldPoint {
        x: x * size,
        y: mercator_y.clamp(0.0, 1.0) * size,
    }
}

/// Converts world pixels back to a geographical point.
pub fn world_to_geo(point: WorldPoint, zoom: f64) -> GeoPoint {
    let size = world_size(zoom);
    let normalized_x = (point.x / size).rem_euclid(1.0);
    let normalized_y = (point.y / size).clamp(0.0, 1.0);
    let longitude = normalized_x * 360.0 - 180.0;
    let mercator_y = std::f64::consts::PI * (1.0 - 2.0 * normalized_y);
    let latitude = mercator_y.sinh().atan().to_degrees();

    GeoPoint::new(latitude, longitude)
}

/// Converts a point to a standard integer XYZ tile coordinate.
pub fn geo_to_tile(point: GeoPoint, zoom: u8) -> TileCoordinate {
    let count = TileCoordinate::tile_count(zoom).unwrap_or(u32::MAX);
    let world = geo_to_world(point, f64::from(zoom));
    let size = world_size(f64::from(zoom));
    let x = (((world.x / size) * f64::from(count)).floor() as i64).clamp(0, i64::from(count) - 1);
    let y = ((world.y / size) * f64::from(count)).floor() as i64;

    TileCoordinate::from_wrapped(x, y.clamp(0, i64::from(count) - 1), zoom)
        .unwrap_or_else(|| TileCoordinate::new(0, 0, zoom))
}

/// Returns the top-left world pixel of a tile at its own integer zoom.
pub fn tile_to_world(tile: TileCoordinate) -> WorldPoint {
    WorldPoint {
        x: f64::from(tile.x) * TILE_SIZE,
        y: f64::from(tile.y) * TILE_SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f64, right: f64, epsilon: f64) {
        assert!((left - right).abs() <= epsilon, "{left} != {right}");
    }

    #[test]
    fn world_size_doubles_per_zoom() {
        assert_eq!(world_size(0.0), TILE_SIZE);
        assert_eq!(world_size(1.0), TILE_SIZE * 2.0);
        close(world_size(12.5), TILE_SIZE * 2.0_f64.powf(12.5), 1e-8);
    }

    #[test]
    fn known_equator_and_meridian() {
        let world = geo_to_world(GeoPoint::new(0.0, 0.0), 2.0);
        close(world.x, 512.0, 1e-10);
        close(world.y, 512.0, 1e-10);
    }

    #[test]
    fn geographic_round_trip_is_stable() {
        for point in [
            GeoPoint::new(-12.0464, -77.0428),
            GeoPoint::new(51.5074, -0.1278),
            GeoPoint::new(85.0, 179.5),
            GeoPoint::new(-85.0, -179.5),
        ] {
            let result = world_to_geo(geo_to_world(point, 12.0), 12.0);
            close(result.latitude, point.latitude, 1e-10);
            close(result.longitude, point.longitude, 1e-10);
        }
    }

    #[test]
    fn tile_edges_match_xyz() {
        assert_eq!(
            geo_to_tile(GeoPoint::new(0.0, -180.0), 2),
            TileCoordinate::new(0, 2, 2)
        );
        assert_eq!(
            geo_to_tile(GeoPoint::new(0.0, 180.0), 2),
            TileCoordinate::new(3, 2, 2)
        );
        assert_eq!(geo_to_tile(GeoPoint::new(MAX_LATITUDE, 0.0), 2).y, 0);
        assert_eq!(geo_to_tile(GeoPoint::new(MIN_LATITUDE, 0.0), 2).y, 3);
    }

    #[test]
    fn tile_to_world_uses_nominal_tile_size() {
        assert_eq!(
            tile_to_world(TileCoordinate::new(3, 2, 2)),
            WorldPoint { x: 768.0, y: 512.0 }
        );
    }
}
