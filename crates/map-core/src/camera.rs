use crate::{GeoPoint, ScreenPoint, Viewport, geo_to_world, world_to_geo};

/// Minimum supported camera zoom.
pub const MIN_ZOOM: f64 = 2.0;
/// Maximum supported camera zoom.
pub const MAX_ZOOM: f64 = 19.0;
/// Initial camera zoom.
pub const DEFAULT_ZOOM: f64 = 12.0;

/// North-up 2D map camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapCamera {
    pub center: GeoPoint,
    pub zoom: f64,
}

impl MapCamera {
    pub fn new(center: GeoPoint, zoom: f64) -> Self {
        Self {
            center: center.normalized(),
            zoom: clamp_zoom(zoom),
        }
    }

    pub fn set_center(&mut self, center: GeoPoint) {
        self.center = center.normalized();
    }

    pub fn set_zoom(&mut self, zoom: f64) {
        self.zoom = clamp_zoom(zoom);
    }

    pub fn zoom_by(&mut self, delta: f64) {
        self.set_zoom(self.zoom + delta);
    }

    /// Pans the map by a screen-space drag delta. Dragging right moves the map
    /// center west, matching normal desktop map behavior.
    pub fn pan_by_screen_delta(&mut self, delta: ScreenPoint, _viewport: Viewport) {
        let center_world = geo_to_world(self.center, self.zoom);
        let moved_center = crate::WorldPoint {
            x: center_world.x - f64::from(delta.x),
            y: center_world.y - f64::from(delta.y),
        };
        self.center = world_to_geo(moved_center, self.zoom);
    }

    /// Changes zoom while keeping the geographical point below `cursor`
    /// stationary. This is the behavior users expect from a map wheel.
    pub fn zoom_towards_screen(&mut self, new_zoom: f64, cursor: ScreenPoint, viewport: Viewport) {
        let anchor = viewport.screen_to_geo(self, cursor);
        self.set_zoom(new_zoom);

        let anchor_world = geo_to_world(anchor, self.zoom);
        let center_world = crate::WorldPoint {
            x: anchor_world.x - f64::from(cursor.x - viewport.width / 2.0),
            y: anchor_world.y - f64::from(cursor.y - viewport.height / 2.0),
        };
        self.center = world_to_geo(center_world, self.zoom);
    }

    pub fn geo_to_screen(&self, point: GeoPoint, viewport: Viewport) -> ScreenPoint {
        viewport.geo_to_screen(self, point)
    }

    pub fn screen_to_geo(&self, point: ScreenPoint, viewport: Viewport) -> GeoPoint {
        viewport.screen_to_geo(self, point)
    }

    pub fn visible_tiles(&self, viewport: Viewport) -> Vec<crate::TileCoordinate> {
        viewport.visible_tiles(self)
    }
}

fn clamp_zoom(zoom: f64) -> f64 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        DEFAULT_ZOOM
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeoPoint, Viewport};

    #[test]
    fn zoom_is_clamped() {
        let mut camera = MapCamera::new(GeoPoint::new(0.0, 0.0), 100.0);
        assert_eq!(camera.zoom, MAX_ZOOM);
        camera.set_zoom(f64::NAN);
        assert_eq!(camera.zoom, DEFAULT_ZOOM);
    }

    #[test]
    fn drag_moves_center_opposite_to_pointer() {
        let viewport = Viewport::new(800.0, 600.0).expect("valid viewport");
        let mut camera = MapCamera::new(GeoPoint::new(0.0, 0.0), 2.0);
        camera.pan_by_screen_delta(ScreenPoint { x: 100.0, y: 0.0 }, viewport);
        assert!(camera.center.longitude < 0.0);
    }

    #[test]
    fn cursor_zoom_keeps_anchor_fixed() {
        let viewport = Viewport::new(800.0, 600.0).expect("valid viewport");
        let cursor = ScreenPoint { x: 120.0, y: 220.0 };
        let mut camera = MapCamera::new(GeoPoint::new(0.0, 0.0), 8.0);
        let before = camera.screen_to_geo(cursor, viewport);
        camera.zoom_towards_screen(10.0, cursor, viewport);
        let after = camera.screen_to_geo(cursor, viewport);
        assert!((before.latitude - after.latitude).abs() < 1e-9);
        assert!((before.longitude - after.longitude).abs() < 1e-9);
    }
}
