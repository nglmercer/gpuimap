//! Location domain types and platform-specific sources.

mod mock;
mod source;

pub use mock::MockLocationSource;
pub use source::{
    LocationBackend, LocationError, LocationEvent, LocationFix, LocationSink, LocationSource,
    LocationState, PermissionStatus,
};

#[cfg(windows)]
pub mod windows;
