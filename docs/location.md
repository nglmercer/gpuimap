# Location

The UI talks to `LocationSource` and receives `LocationFix` values. A fix carries position, optional accuracy/altitude/speed/heading, and a timestamp. `MockLocationSource` is used for deterministic development and tests.

On Windows, `WindowsLocationSource` wraps `Windows.Devices.Geolocation.Geolocator`. Permission is requested only from the foreground UI flow, because Windows requires `RequestAccessAsync` to run while the application is foregrounded and on the UI thread. The request and first-position lookup use completion callbacks so the GPUI thread never waits on WinRT operations. The UI also exposes a location-privacy settings action for a previously denied permission or disabled location service. The packaged-app location capability is a distribution concern and is not needed for the unpackaged development executable.
