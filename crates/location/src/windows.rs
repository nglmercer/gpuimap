//! Windows Runtime `Geolocator` adapter.
//!
//! This module is compiled only for Windows. No Windows types escape this
//! module: callers receive the platform-neutral `LocationFix` and events.

use std::sync::{Arc, Mutex};

use windows::{
    Devices::Geolocation::{
        GeolocationAccessStatus, Geolocator, PositionChangedEventArgs, PositionStatus,
        StatusChangedEventArgs,
    },
    Foundation::TypedEventHandler,
};

use crate::{
    LocationBackend, LocationError, LocationEvent, LocationFix, LocationFixSink, LocationSink,
    LocationSource, LocationState, PermissionResultSink, PermissionStatus,
};

pub struct WindowsLocationSource {
    locator: Option<Geolocator>,
    position_token: Option<i64>,
    status_token: Option<i64>,
    sink: Option<LocationSink>,
    permission: Arc<Mutex<PermissionStatus>>,
    last_fix: Arc<Mutex<Option<LocationFix>>>,
}

impl WindowsLocationSource {
    pub fn new() -> Self {
        Self {
            locator: None,
            position_token: None,
            status_token: None,
            sink: None,
            permission: Arc::new(Mutex::new(PermissionStatus::Unspecified)),
            last_fix: Arc::new(Mutex::new(None)),
        }
    }

    fn permission_status(&self) -> PermissionStatus {
        self.permission
            .lock()
            .map(|permission| *permission)
            .unwrap_or(PermissionStatus::Unspecified)
    }

    fn set_permission_status(&self, permission: PermissionStatus) {
        if let Ok(mut current) = self.permission.lock() {
            *current = permission;
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
        let permission = permission_from_access(result);
        self.set_permission_status(permission);
        if permission == PermissionStatus::Allowed {
            let _ = self.ensure_locator()?;
        }
        Ok(permission)
    }

    fn request_permission_async(
        &mut self,
        sink: PermissionResultSink,
    ) -> Result<(), LocationError> {
        let permission_state = Arc::clone(&self.permission);
        Geolocator::RequestAccessAsync()
            .map_err(backend_error)?
            .when(move |result| {
                let result = result.map(permission_from_access).map_err(backend_error);
                if let Ok(permission) = &result
                    && let Ok(mut current) = permission_state.lock()
                {
                    *current = *permission;
                }
                sink(result);
            })
            .map_err(backend_error)
    }

    fn current_position(&mut self) -> Result<LocationFix, LocationError> {
        if self.permission_status() != PermissionStatus::Allowed {
            return Err(LocationError::PermissionDenied);
        }
        self.last_fix
            .lock()
            .ok()
            .and_then(|fix| fix.clone())
            .ok_or(LocationError::NoData)
    }

    fn request_current_position_async(
        &mut self,
        sink: LocationFixSink,
    ) -> Result<(), LocationError> {
        if self.permission_status() != PermissionStatus::Allowed {
            return Err(LocationError::PermissionDenied);
        }
        let locator = self.ensure_locator()?.clone();
        let last_fix = Arc::clone(&self.last_fix);
        locator
            .GetGeopositionAsync()
            .map_err(backend_error)?
            .when(move |result| {
                let result = result
                    .map_err(backend_error)
                    .and_then(|position| fix_from_position(&position));
                if let Ok(fix) = &result
                    && let Ok(mut current) = last_fix.lock()
                {
                    *current = Some(fix.clone());
                }
                sink(result);
            })
            .map_err(backend_error)
    }

    fn open_location_settings(&mut self) -> Result<(), LocationError> {
        std::process::Command::new("explorer.exe")
            .arg("ms-settings:privacy-location")
            .spawn()
            .map(|_| ())
            .map_err(|error| LocationError::Backend(error.to_string()))
    }

    fn start_updates(&mut self, sink: LocationSink) -> Result<(), LocationError> {
        if self.permission_status() != PermissionStatus::Allowed {
            return Err(LocationError::PermissionDenied);
        }
        self.stop_updates();
        let locator = self.ensure_locator()?.clone();
        let event_sink = Arc::clone(&sink);
        let last_fix = Arc::clone(&self.last_fix);
        let handler =
            TypedEventHandler::<Geolocator, PositionChangedEventArgs>::new(move |_sender, args| {
                if let Some(args) = args.as_ref() {
                    match args
                        .Position()
                        .map_err(backend_error)
                        .and_then(|position| fix_from_position(&position))
                    {
                        Ok(fix) => {
                            if let Ok(mut current) = last_fix.lock() {
                                *current = Some(fix.clone());
                            }
                            event_sink(LocationEvent::Fix(fix));
                        }
                        Err(error) => {
                            event_sink(LocationEvent::State(LocationState::Unavailable(error)))
                        }
                    }
                }
                Ok(())
            });
        let position_token = locator.PositionChanged(&handler).map_err(backend_error)?;

        let status_sink = Arc::clone(&sink);
        let status_handler =
            TypedEventHandler::<Geolocator, StatusChangedEventArgs>::new(move |_sender, args| {
                if let Some(args) = args.as_ref() {
                    match args.Status().map_err(backend_error) {
                        Ok(status) => {
                            if let Some(event) = status_event(status) {
                                status_sink(event);
                            }
                        }
                        Err(error) => {
                            status_sink(LocationEvent::State(LocationState::Unavailable(error)))
                        }
                    }
                }
                Ok(())
            });
        let status_token = match locator
            .StatusChanged(&status_handler)
            .map_err(backend_error)
        {
            Ok(token) => token,
            Err(error) => {
                let _ = locator.RemovePositionChanged(position_token);
                return Err(error);
            }
        };
        let initial_status = match locator.LocationStatus().map_err(backend_error) {
            Ok(status) => status,
            Err(error) => {
                let _ = locator.RemovePositionChanged(position_token);
                let _ = locator.RemoveStatusChanged(status_token);
                return Err(error);
            }
        };

        self.position_token = Some(position_token);
        self.status_token = Some(status_token);
        self.sink = Some(sink);
        if let Some(event) = status_event(initial_status)
            && let Some(sink) = &self.sink
        {
            sink(event);
        }
        Ok(())
    }

    fn stop_updates(&mut self) {
        if let Some(locator) = &self.locator {
            if let Some(token) = self.position_token.take() {
                let _ = locator.RemovePositionChanged(token);
            }
            if let Some(token) = self.status_token.take() {
                let _ = locator.RemoveStatusChanged(token);
            }
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
    if error.code().0 == 0x8007_05B4u32 as i32 {
        LocationError::NoData
    } else {
        LocationError::Backend(error.to_string())
    }
}

fn permission_from_access(status: GeolocationAccessStatus) -> PermissionStatus {
    match status {
        GeolocationAccessStatus::Allowed => PermissionStatus::Allowed,
        GeolocationAccessStatus::Denied => PermissionStatus::Denied,
        GeolocationAccessStatus::Unspecified => PermissionStatus::Unspecified,
        _ => PermissionStatus::Unspecified,
    }
}

fn status_event(status: PositionStatus) -> Option<LocationEvent> {
    let state = match status {
        PositionStatus::Initializing => LocationState::Searching,
        PositionStatus::NoData => LocationState::Unavailable(LocationError::NoData),
        PositionStatus::Disabled => LocationState::Unavailable(LocationError::Disabled),
        PositionStatus::NotAvailable => LocationState::Unavailable(LocationError::NotSupported),
        PositionStatus::Ready | PositionStatus::NotInitialized => return None,
        _ => LocationState::Unavailable(LocationError::Unavailable),
    };
    Some(LocationEvent::State(state))
}
