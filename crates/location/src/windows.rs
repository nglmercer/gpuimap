//! Windows Runtime `Geolocator` adapter.
//!
//! This module is compiled only for Windows. No Windows types escape this
//! module: callers receive the platform-neutral `LocationFix` and events.

use std::sync::Arc;

use windows::{
    Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionChangedEventArgs},
    Foundation::TypedEventHandler,
};

use crate::{
    LocationBackend, LocationError, LocationEvent, LocationFix, LocationSink, LocationSource,
    LocationState, PermissionStatus,
};

pub struct WindowsLocationSource {
    locator: Option<Geolocator>,
    position_token: Option<i64>,
    sink: Option<LocationSink>,
    permission: PermissionStatus,
}

impl WindowsLocationSource {
    pub fn new() -> Self {
        Self {
            locator: None,
            position_token: None,
            sink: None,
            permission: PermissionStatus::Unspecified,
        }
    }

    fn ensure_locator(&mut self) -> Result<&Geolocator, LocationError> {
        if self.locator.is_none() {
            self.locator = Some(Geolocator::new().map_err(backend_error)?);
        }
        self.locator
            .as_ref()
            .ok_or_else(|| LocationError::Backend("Geolocator was not created".into()))
    }
}

impl Default for WindowsLocationSource {
    fn default() -> Self {
        Self::new()
    }
}

impl LocationSource for WindowsLocationSource {
    fn backend(&self) -> LocationBackend {
        LocationBackend::Windows
    }

    /// Must be called from the foreground UI thread. The UI owns this flow
    /// instead of silently requesting permission from a worker.
    fn request_permission(&mut self) -> Result<PermissionStatus, LocationError> {
        let result = Geolocator::RequestAccessAsync()
            .map_err(backend_error)?
            .join()
            .map_err(backend_error)?;
        let permission = match result {
            GeolocationAccessStatus::Allowed => PermissionStatus::Allowed,
            GeolocationAccessStatus::Denied => PermissionStatus::Denied,
            _ => PermissionStatus::Unspecified,
        };
        self.permission = permission;
        if permission == PermissionStatus::Allowed {
            let _ = self.ensure_locator()?;
        }
        Ok(permission)
    }

    fn current_position(&mut self) -> Result<LocationFix, LocationError> {
        if self.permission != PermissionStatus::Allowed {
            return Err(LocationError::PermissionDenied);
        }
        let position = self
            .ensure_locator()?
            .GetGeopositionAsync()
            .map_err(backend_error)?
            .join()
            .map_err(backend_error)?;
        fix_from_position(&position)
    }

    fn start_updates(&mut self, sink: LocationSink) -> Result<(), LocationError> {
        if self.permission != PermissionStatus::Allowed {
            return Err(LocationError::PermissionDenied);
        }
        self.stop_updates();
        let locator = self.ensure_locator()?.clone();
        let event_sink = Arc::clone(&sink);
        let handler =
            TypedEventHandler::<Geolocator, PositionChangedEventArgs>::new(move |_sender, args| {
                if let Some(args) = args.as_ref() {
                    match args
                        .Position()
                        .map_err(backend_error)
                        .and_then(|position| fix_from_position(&position))
                    {
                        Ok(fix) => event_sink(LocationEvent::Fix(fix)),
                        Err(error) => {
                            event_sink(LocationEvent::State(LocationState::Unavailable(error)))
                        }
                    }
                }
                Ok(())
            });
        let token = locator.PositionChanged(&handler).map_err(backend_error)?;
        self.position_token = Some(token);
        self.sink = Some(sink);
        Ok(())
    }

    fn stop_updates(&mut self) {
        if let (Some(locator), Some(token)) = (&self.locator, self.position_token.take()) {
            let _ = locator.RemovePositionChanged(token);
        }
        self.sink = None;
    }
}

impl Drop for WindowsLocationSource {
    fn drop(&mut self) {
        self.stop_updates();
    }
}

fn fix_from_position(
    position: &windows::Devices::Geolocation::Geoposition,
) -> Result<LocationFix, LocationError> {
    let coordinate = position.Coordinate().map_err(backend_error)?;
    let latitude = coordinate.Latitude().map_err(backend_error)?;
    let longitude = coordinate.Longitude().map_err(backend_error)?;
    let point = map_core::GeoPoint::try_new(latitude, longitude)
        .map_err(|error| LocationError::InvalidFix(error.to_string()))?;
    let mut fix = LocationFix::new(point);
    if let Ok(accuracy) = coordinate.Accuracy()
        && accuracy.is_finite()
        && accuracy >= 0.0
    {
        fix.horizontal_accuracy_m = Some(accuracy);
    }
    if let Ok(altitude) = coordinate.Altitude().and_then(|value| value.Value())
        && altitude.is_finite()
    {
        fix.altitude_m = Some(altitude);
    }
    if let Ok(speed) = coordinate.Speed().and_then(|value| value.Value())
        && speed.is_finite()
        && speed >= 0.0
    {
        fix.speed_mps = Some(speed);
    }
    if let Ok(heading) = coordinate.Heading().and_then(|value| value.Value())
        && heading.is_finite()
        && (0.0..360.0).contains(&heading)
    {
        fix.heading_deg = Some(heading);
    }
    Ok(fix)
}

fn backend_error(error: windows::core::Error) -> LocationError {
    LocationError::Backend(error.to_string())
}
