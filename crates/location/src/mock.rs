use std::sync::Arc;

use map_core::GeoPoint;

use crate::{
    LocationBackend, LocationError, LocationEvent, LocationFix, LocationSink, LocationSource,
    PermissionStatus,
};

/// Deterministic source for UI development and tests without GPS hardware.
pub struct MockLocationSource {
    permission: PermissionStatus,
    fix: Option<LocationFix>,
    sink: Option<LocationSink>,
}

impl MockLocationSource {
    pub fn fixed(position: GeoPoint) -> Self {
        Self {
            permission: PermissionStatus::Allowed,
            fix: Some(LocationFix::new(position)),
            sink: None,
        }
    }

    pub fn unavailable() -> Self {
        Self {
            permission: PermissionStatus::Allowed,
            fix: None,
            sink: None,
        }
    }

    pub fn with_permission(mut self, permission: PermissionStatus) -> Self {
        self.permission = permission;
        self
    }

    /// Changes the simulated position and emits a fix when updates are active.
    pub fn set_fix(&mut self, fix: LocationFix) {
        self.fix = Some(fix.clone());
        if let Some(sink) = &self.sink {
            sink(LocationEvent::Fix(fix));
        }
    }

    pub fn clear_fix(&mut self) {
        self.fix = None;
        if let Some(sink) = &self.sink {
            sink(LocationEvent::State(crate::LocationState::Searching));
        }
    }
}

impl LocationSource for MockLocationSource {
    fn backend(&self) -> LocationBackend {
        LocationBackend::Simulated
    }

    fn request_permission(&mut self) -> Result<PermissionStatus, LocationError> {
        Ok(self.permission)
    }

    fn current_position(&mut self) -> Result<LocationFix, LocationError> {
        match self.permission {
            PermissionStatus::Denied => return Err(LocationError::PermissionDenied),
            PermissionStatus::Unspecified => return Err(LocationError::Unavailable),
            PermissionStatus::Allowed => {}
        }
        self.fix.clone().ok_or(LocationError::Unavailable)
    }

    fn start_updates(&mut self, sink: LocationSink) -> Result<(), LocationError> {
        self.sink = Some(Arc::clone(&sink));
        if let Some(fix) = &self.fix {
            sink(LocationEvent::Fix(fix.clone()));
        } else {
            sink(LocationEvent::State(crate::LocationState::Searching));
        }
        Ok(())
    }

    fn stop_updates(&mut self) {
        self.sink = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn fixed_source_returns_and_emits_same_position() {
        let position = GeoPoint::new(-12.0464, -77.0428);
        let mut source = MockLocationSource::fixed(position);
        assert_eq!(source.current_position().expect("fix").position, position);

        let events = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&events);
        source
            .start_updates(Arc::new(move |event| {
                received.lock().expect("events").push(event)
            }))
            .expect("updates");
        assert_eq!(events.lock().expect("events").len(), 1);
    }

    #[test]
    fn denied_source_does_not_expose_fix() {
        let mut source = MockLocationSource::fixed(GeoPoint::new(0.0, 0.0))
            .with_permission(PermissionStatus::Denied);
        assert_eq!(
            source.current_position().expect_err("permission error"),
            LocationError::PermissionDenied
        );
    }
}
