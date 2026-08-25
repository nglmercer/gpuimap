use std::fmt;

/// The latitude limits of Web Mercator (EPSG:3857).
pub const MIN_LATITUDE: f64 = -85.051_128_78;
pub const MAX_LATITUDE: f64 = 85.051_128_78;

/// A geographical position in decimal degrees.
///
/// Latitude is north/south and longitude is east/west. `new` clamps latitude
/// to the Web Mercator domain and wraps longitude into `[-180, 180]`. Use
/// `try_new` when invalid finite input should be rejected instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint {
    pub latitude: f64,
    pub longitude: f64,
}

impl GeoPoint {
    /// Creates a point suitable for Web Mercator projection.
    pub fn new(latitude: f64, longitude: f64) -> Self {
        Self {
            latitude: if latitude.is_finite() {
                latitude.clamp(MIN_LATITUDE, MAX_LATITUDE)
            } else {
                0.0
            },
            longitude: if longitude.is_finite() {
                normalize_longitude(longitude)
            } else {
                0.0
            },
        }
    }

    /// Creates a point while rejecting non-finite or out-of-domain latitude.
    pub fn try_new(latitude: f64, longitude: f64) -> Result<Self, CoordinateError> {
        if !latitude.is_finite() || !longitude.is_finite() {
            return Err(CoordinateError::NonFinite);
        }
        if !(MIN_LATITUDE..=MAX_LATITUDE).contains(&latitude) {
            return Err(CoordinateError::LatitudeOutOfRange { latitude });
        }

        Ok(Self {
            latitude,
            longitude: normalize_longitude(longitude),
        })
    }

    /// Returns the longitude wrapped to the conventional `[-180, 180]` range.
    pub fn normalized(self) -> Self {
        Self {
            longitude: normalize_longitude(self.longitude),
            ..self
        }
    }
}

/// A point in the projected world-pixel coordinate system.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldPoint {
    pub x: f64,
    pub y: f64,
}

/// A point in logical viewport pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScreenPoint {
    pub x: f32,
    pub y: f32,
}

/// An integer XYZ raster tile coordinate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TileCoordinate {
    pub x: u32,
    pub y: u32,
    pub zoom: u8,
}

impl TileCoordinate {
    pub const fn new(x: u32, y: u32, zoom: u8) -> Self {
        Self { x, y, zoom }
    }

    /// Number of tiles along one axis at a zoom level.
    pub fn tile_count(zoom: u8) -> Option<u32> {
        if zoom >= 32 { None } else { Some(1u32 << zoom) }
    }

    /// Returns whether this coordinate is inside its zoom-level tile matrix.
    pub fn is_valid(self) -> bool {
        Self::tile_count(self.zoom).is_some_and(|count| self.x < count && self.y < count)
    }

    /// Wraps X around the antimeridian and rejects Y outside the matrix.
    pub fn from_wrapped(x: i64, y: i64, zoom: u8) -> Option<Self> {
        let count = i64::from(Self::tile_count(zoom)?);
        if !(0..count).contains(&y) {
            return None;
        }

        Some(Self {
            x: x.rem_euclid(count) as u32,
            y: y as u32,
            zoom,
        })
    }
}

impl fmt::Display for TileCoordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}/{}", self.zoom, self.x, self.y)
    }
}

/// Errors returned by strict coordinate construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CoordinateError {
    NonFinite,
    LatitudeOutOfRange { latitude: f64 },
}

impl fmt::Display for CoordinateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite => formatter.write_str("coordinate values must be finite"),
            Self::LatitudeOutOfRange { latitude } => {
                write!(
                    formatter,
                    "latitude {latitude} is outside Web Mercator bounds"
                )
            }
        }
    }
}

impl std::error::Error for CoordinateError {}

/// Wraps an arbitrary finite longitude while preserving `180` when supplied
/// exactly. The projection itself remains continuous at the antimeridian.
pub(crate) fn normalize_longitude(longitude: f64) -> f64 {
    if (-180.0..=180.0).contains(&longitude) {
        longitude
    } else {
        let normalized = (longitude + 180.0).rem_euclid(360.0) - 180.0;
        if normalized == -180.0 && longitude > 0.0 {
            180.0
        } else {
            normalized
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_clamps_and_wraps() {
        let point = GeoPoint::new(90.0, 540.0);
        assert_eq!(point.latitude, MAX_LATITUDE);
        assert_eq!(point.longitude, 180.0);
    }

    #[test]
    fn strict_constructor_rejects_invalid_latitude() {
        assert!(matches!(
            GeoPoint::try_new(90.0, 0.0),
            Err(CoordinateError::LatitudeOutOfRange { .. })
        ));
        assert_eq!(GeoPoint::try_new(12.0, 540.0).unwrap().longitude, 180.0);
    }

    #[test]
    fn tile_coordinates_wrap_x_but_clip_y() {
        assert_eq!(
            TileCoordinate::from_wrapped(-1, 2, 2),
            Some(TileCoordinate::new(3, 2, 2))
        );
        assert_eq!(TileCoordinate::from_wrapped(0, -1, 2), None);
        assert!(!TileCoordinate::new(4, 0, 2).is_valid());
    }
}
