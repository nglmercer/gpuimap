# Architecture

```text
GPUI UI
  ├── toolbar, status bar, map viewport
  └── user interaction
        │
        ▼
Application state
  ├── MapCamera / Viewport / FollowMode
  ├── location::LocationFix
  └── map-tiles::TileScheduler
        ├── memory cache
        ├── disk cache
        └── provider + HTTP workers

map-core
  └── pure coordinate, projection, camera, and tile-selection math

location
  ├── MockLocationSource
  └── WindowsLocationSource (Windows only)
```

The dependency direction is intentionally one-way:

```text
ui → map-core, map-tiles, location, storage
map-tiles → map-core, storage
location → map-core
```

The map engine never calls a platform API and no worker calls `render()` or owns a GPUI entity.

