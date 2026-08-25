use std::{fmt, sync::Arc, time::SystemTime};

use map_core::GeoPoint;

/// Identifies the source behind a location fix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocationBackend {
    Windows,
    NmeaSerial,
    Simulated,
}

/// Permission result from the platform location service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionStatus {
    Allowed,
    Denied,
    Unspecified,
}

/// A single normalized location fix.
#[derive(Clone, Debug, PartialEq)]
pub struct LocationFix {
    pub position: GeoPoint,
    pub horizontal_accuracy_m: Option<f64>,
    pub altitude_m: Option<f64>,
    pub speed_mps: Option<f64>,
    pub heading_deg: Option<f64>,
    pub timestamp: SystemTime,
}

impl LocationFix {
    pub fn new(position: GeoPoint) -> Self {
        Self {
            position,
            horizontal_accuracy_m: None,
            altitude_m: None,
            speed_mps: None,
            heading_deg: None,
            timestamp: SystemTime::now(),
        }
    }

    pub fn with_accuracy(mut self, horizontal_accuracy_m: f64) -> Self {
        if horizontal_accuracy_m.is_finite() && horizontal_accuracy_m >= 0.0 {
            self.horizontal_accuracy_m = Some(horizontal_accuracy_m);
        }
        self
    }
}

/// Events delivered by a source while continuous updates are active.
#[derive(Clone, Debug, PartialEq)]
pub enum LocationEvent {
    Permission(Result<PermissionStatus, LocationError>),
    Fix(LocationFix),
    State(LocationState),
}

/// Explicit location state for UI presentation and diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub enum LocationState {
    Disabled,
    RequestingPermission,
    PermissionDenied,
    Searching,
    Available(LocationFix),
    Unavailable(LocationError),
}

/// Errors at the location-domain boundary.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LocationError {
    #[error("location permission was denied")]
    PermissionDenied,
    #[error("location services are disabled")]
    Disabled,
    #[error("no location data is available")]
    NoData,
    #[error("location data is unavailable")]
    Unavailable,
    #[error("location backend is not supported on this platform")]
    NotSupported,
    #[error("location backend failed: {0}")]
    Backend(String),
    #[error("location fix is invalid: {0}")]
    InvalidFix(String),
}

/// Callback used by continuous location updates.
pub type LocationSink = Arc<dyn Fn(LocationEvent) + Send + Sync + 'static>;

/// Completion callback for a non-blocking permission request.
pub type PermissionResultSink =
    Arc<dyn Fn(Result<PermissionStatus, LocationError>) + Send + Sync + 'static>;

/// Completion callback for a non-blocking one-shot location request.
pub type LocationFixSink = Arc<dyn Fn(Result<LocationFix, LocationError>) + Send + Sync + 'static>;

/// Platform adapter consumed by the application without exposing platform
/// types. Permission is deliberately an explicit call so the UI can invoke it
/// while foregrounded, as required by Windows.
pub trait LocationSource: 'static {
    fn backend(&self) -> LocationBackend;
    fn request_permission(&mut self) -> Result<PermissionStatus, LocationError>;
    fn current_position(&mut self) -> Result<LocationFix, LocationError>;

    /// Starts a permission request without blocking the UI thread. Windows
    /// requires the operation to be initiated from the foreground UI thread,
    /// while its completion callback may run later.
    fn request_permission_async(
        &mut self,
        sink: PermissionResultSink,
    ) -> Result<(), LocationError> {
        sink(self.request_permission());
        Ok(())
    }

    /// Starts a one-shot position request without blocking the UI thread.
    fn request_current_position_async(
        &mut self,
        sink: LocationFixSink,
    ) -> Result<(), LocationError> {
        sink(self.current_position());
        Ok(())
    }

    /// Opens the platform location privacy settings when supported.
    fn open_location_settings(&mut self) -> Result<(), LocationError> {
        Err(LocationError::NotSupported)
    }

    fn start_updates(&mut self, sink: LocationSink) -> Result<(), LocationError>;
    fn stop_updates(&mut self);
}

impl fmt::Display for LocationBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Windows => "Windows Location",
            Self::NmeaSerial => "NMEA serial GPS",
            Self::Simulated => "Simulated",
        };
        formatter.write_str(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_defaults_to_unknown_optional_fields() {
        let fix = LocationFix::new(GeoPoint::new(-12.0464, -77.0428));
        assert!(fix.horizontal_accuracy_m.is_none());
        assert!(fix.altitude_m.is_none());
        assert!(fix.speed_mps.is_none());
        assert!(fix.heading_deg.is_none());
    }

    #[test]
    fn invalid_accuracy_is_not_recorded() {
        let fix = LocationFix::new(GeoPoint::new(0.0, 0.0)).with_accuracy(-1.0);
        assert!(fix.horizontal_accuracy_m.is_none());
    }
}
