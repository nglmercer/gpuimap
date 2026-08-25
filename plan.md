Yes. I’d build this as a **Windows-first native Rust desktop map application using GPUI**, with Windows location/GPS behind an abstraction so Linux/macOS can be added later without rewriting the map.

A key current detail: GPUI’s older README still says macOS/Linux and warns that it is pre-1.0, but the current Zed source has a dedicated `gpui_windows` crate and a DirectX Windows renderer, and current Zed Windows builds are running through that backend. So Windows is viable, but we should **pin GPUI versions/revisions and isolate GPUI-specific code** because the API is still evolving. ([GitHub][1])

# Project plan: `gpui-map`

## 1. Project objective

Build a native desktop application that can:

* Run first on **Windows 10/11 x86_64**.
* Render an interactive map inside GPUI.
* Pan with mouse drag.
* Zoom with mouse wheel/buttons.
* Load map tiles asynchronously.
* Cache tiles locally.
* Obtain the user's current Windows location.
* Center the map on the current location.
* Show location accuracy.
* Optionally follow location updates.
* Later support physical GPS/NMEA receivers.
* Later support Linux/macOS without changing map-domain code.

For the first implementation, use **raster XYZ tiles**, not vector tiles.

GPUI already supports images, raw image bytes, image caching, custom painting, paths and canvas rendering, which gives us everything required for an initial raster map implementation. ([GitHub][2])

---

# 2. Architecture decision

Use this layered architecture:

```text
┌─────────────────────────────────────────────┐
│                 GPUI UI                     │
│                                             │
│ Toolbar / Status / Search / MapViewport     │
└─────────────────────┬───────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────┐
│               Application State             │
│                                             │
│ camera / location / selected marker / mode  │
└─────────┬───────────────────┬───────────────┘
          │                   │
          ▼                   ▼
┌──────────────────┐   ┌──────────────────────┐
│   Map Engine     │   │ Location Service     │
│                  │   │                      │
│ projection       │   │ trait LocationSource│
│ tile math        │   │                      │
│ viewport         │   └──────────┬───────────┘
│ markers          │              │
└───────┬──────────┘      ┌───────┴─────────┐
        │                 │                 │
        ▼                 ▼                 ▼
┌────────────────┐   Windows WinRT     Future NMEA
│ Tile Service   │   Geolocator        serial GPS
│                │
│ HTTP           │
│ memory cache   │
│ disk cache     │
└────────────────┘
```

The crucial rule is:

> **Map logic must never call Windows APIs or GPUI directly.**

Likewise:

> **Windows location code must never know anything about tiles or rendering.**

That separation will make the project much easier for LLMs to modify safely.

---

# 3. Recommended workspace

Start as a Cargo workspace rather than one enormous crate. The official `create-gpui-app` utility supports workspace creation. ([GitHub][3])

```text
gpui-map/
│
├── Cargo.toml
├── rust-toolchain.toml
├── README.md
├── LICENSE
│
├── crates/
│   │
│   ├── app/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── app.rs
│   │       └── actions.rs
│   │
│   ├── ui/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── main_window.rs
│   │       ├── toolbar.rs
│   │       ├── status_bar.rs
│   │       └── map_view.rs
│   │
│   ├── map-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── coordinate.rs
│   │       ├── camera.rs
│   │       ├── mercator.rs
│   │       ├── tile.rs
│   │       └── viewport.rs
│   │
│   ├── map-tiles/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── provider.rs
│   │       ├── client.rs
│   │       ├── cache.rs
│   │       └── scheduler.rs
│   │
│   ├── location/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── source.rs
│   │       ├── mock.rs
│   │       └── windows.rs
│   │
│   └── storage/
│       └── src/
│           ├── lib.rs
│           └── paths.rs
│
├── tests/
│
└── docs/
    ├── architecture.md
    ├── map-engine.md
    ├── location.md
    └── llm-rules.md
```

This is slightly more structure than an MVP strictly needs, but it prevents the codebase from becoming tangled as soon as GPS and caching arrive.

---

# 4. Core domain specification

## Coordinates

Use explicit types rather than tuples.

```rust
pub struct GeoPoint {
    pub latitude: f64,
    pub longitude: f64,
}

pub struct WorldPoint {
    pub x: f64,
    pub y: f64,
}

pub struct ScreenPoint {
    pub x: f32,
    pub y: f32,
}

pub struct TileCoordinate {
    pub x: u32,
    pub y: u32,
    pub zoom: u8,
}
```

Never pass:

```rust
(f64, f64)
```

for geographical coordinates.

That inevitably produces latitude/longitude ordering bugs.

---

# 5. Map projection

MVP projection:

**Web Mercator / EPSG:3857**

Input:

```text
latitude
longitude
zoom
```

Output:

```text
world pixel position
XYZ tile coordinate
screen pixel position
```

The map engine owns all of these conversions:

```text
GeoPoint
   ↓
Web Mercator
   ↓
WorldPoint
   ↓
Camera
   ↓
ScreenPoint
```

Clamp latitude to approximately:

```text
-85.05112878 .. +85.05112878
```

so Web Mercator math remains valid.

---

# 6. Camera model

```rust
pub struct MapCamera {
    pub center: GeoPoint,
    pub zoom: f64,
}
```

Later:

```rust
pub struct MapCamera {
    pub center: GeoPoint,
    pub zoom: f64,
    pub bearing: f32,
    pub pitch: f32,
}
```

But **do not implement bearing or pitch in MVP**.

Phase 1 map stays north-up and 2D.

Recommended initial limits:

```text
zoom min: 2
zoom max: 19
default: 12
```

Keep fractional zoom internally even if raster tiles are requested at integer zoom levels.

That gives smoother zoom animation later.

---

# 7. Tile rendering design

Use standard XYZ addressing:

```text
/{z}/{x}/{y}.png
```

A viewport determines the visible tiles:

```text
viewport bounds
      +
camera center
      +
zoom
      ↓
visible XYZ tile rectangle
      ↓
TileRequest[]
```

Each tile passes through:

```text
memory cache
     ↓ miss
disk cache
     ↓ miss
HTTP
     ↓
decode PNG
     ↓
GPUI Image
     ↓
render
```

GPUI's current `Image` can be created from encoded image bytes and then converted to renderable image data. ([Docs.rs][4])

---

# 8. Tile provider abstraction

Never hardcode OpenStreetMap URLs throughout the program.

```rust
pub trait TileProvider: Send + Sync {
    fn tile_url(&self, tile: TileCoordinate) -> String;

    fn attribution(&self) -> &str;

    fn max_zoom(&self) -> u8;
}
```

Then:

```rust
struct OpenStreetMapProvider;

struct CustomProvider;

struct LocalTileProvider;
```

This matters because OpenStreetMap explicitly recommends that applications **not hardcode their tile endpoint**. Their standard tile service also requires visible attribution, an identifiable User-Agent and proper caching, and prohibits bulk/offline prefetching. ([OSM Foundation Operations][5])

So use OSM during development, but design production so the provider can be changed by configuration.

---

# 9. Tile cache specification

Two caches.

### L1 memory cache

```text
TileCoordinate → Arc<RenderImage>
```

Use an LRU with a bounded size.

Example target:

```text
128–512 tiles
```

depending on memory.

### L2 disk cache

Something like:

```text
cache/
  tiles/
    osm/
      12/
        1204/
          1534.png
```

Metadata should eventually retain:

```text
ETag
Last-Modified
Expires
Cache-Control
download timestamp
```

Don't build an offline map downloader against OSM's public tile server. Its current policy requires local caching but prohibits bulk downloading/prefetching. ([OSM Foundation Operations][5])

---

# 10. Tile scheduler

Do not initiate HTTP requests from `render()`.

Rendering should be side-effect-light.

Use:

```text
MapView
   ↓ determines wanted tiles
TileScheduler
   ↓
request queue
   ↓
workers
```

Targets:

```text
maximum active tile downloads: 8
visible tiles: highest priority
nearby tiles: optional later
stale requests: cancel or deprioritize
```

If the user moves from Lima to Tokyo, requests for tiles that are no longer useful should not keep dominating the queue.

---

# 11. GPUI MapView

Conceptually:

```rust
pub struct MapView {
    camera: MapCamera,
    viewport: Viewport,
    tile_manager: Entity<TileManager>,
    location: Entity<LocationState>,
    interaction: MapInteraction,
}
```

GPUI views implement `Render`, with their state held by GPUI entities. ([Docs.rs][6])

The map should eventually have two rendering layers:

```text
MapView
│
├── TileLayer
│
├── OverlayCanvas
│   ├── location accuracy circle
│   ├── route/polyline
│   └── debug geometry
│
└── MarkerLayer
```

GPUI's canvas/path APIs are useful for overlays and custom geometry. ([Docs.rs][7])

---

# 12. Location abstraction

This is extremely important.

Define:

```rust
pub trait LocationSource {
    fn request_permission(&mut self) -> ...;

    fn current_position(&mut self) -> ...;

    fn start_updates(&mut self) -> ...;

    fn stop_updates(&mut self);
}
```

Domain result:

```rust
pub struct LocationFix {
    pub position: GeoPoint,
    pub horizontal_accuracy_m: Option<f64>,
    pub altitude_m: Option<f64>,
    pub speed_mps: Option<f64>,
    pub heading_deg: Option<f64>,
    pub timestamp: SystemTime,
}
```

The rest of the application deals only with `LocationFix`.

Never expose Windows types outside `location::windows`.

---

# 13. Windows location implementation

For Windows use:

```text
Windows.Devices.Geolocation.Geolocator
```

rather than the older Win32 COM Location API. Microsoft specifically recommends the WinRT `Windows.Devices.Geolocation` API instead of the older Win32 Location API. ([Microsoft Learn][8])

The Windows implementation should support:

```text
RequestAccessAsync
GetGeopositionAsync
PositionChanged
StatusChanged
```

Microsoft's current API supports both one-time position requests and continuous updates. ([Microsoft Learn][9])

Permission has an important constraint:

> `RequestAccessAsync` must run while the application is foregrounded and from the UI thread. ([Microsoft Learn][10])

Therefore do **not** make location permission happen automatically from a background service.

UI flow:

```text
User clicks "Locate me"
        ↓
Request permission
        ↓
Allowed?
   ┌────┴────┐
  yes       no
   │         │
Get fix    explain +
   │       settings action
   ↓
Center map
```

---

# 14. Windows packaged vs unpackaged

Support unpackaged development first.

Microsoft currently notes that unpackaged desktop applications don't require a `location` capability entry in a package manifest, although they still need to call `RequestAccessAsync`. Packaged applications should declare the location capability. ([Microsoft Learn][11])

So:

### Development

```text
cargo run
unpackaged executable
```

### Distribution later

```text
MSIX
+ location capability
```

Do packaging only after the basic map/location pipeline works.

---

# 15. GPS versus Windows Location

Do **not** start with serial GPS.

Phase 1:

```text
Windows Location API
```

That lets Windows select its available location providers.

Later introduce:

```rust
enum LocationBackend {
    Windows,
    NmeaSerial,
    Simulated,
}
```

Then physical GPS can be:

```text
USB/Bluetooth GPS
      ↓
COM port
      ↓
NMEA 0183
      ↓
NmeaLocationSource
      ↓
LocationFix
```

Because everything returns `LocationFix`, MapView won't care whether the location came from Windows, Wi-Fi positioning, an integrated GPS or an external NMEA receiver.

---

# 16. State machine

Location should have explicit state:

```rust
pub enum LocationState {
    Disabled,
    RequestingPermission,
    PermissionDenied,
    Searching,
    Available(LocationFix),
    Unavailable(LocationError),
}
```

Tile state:

```rust
pub enum TileState {
    Missing,
    Queued,
    Loading,
    Ready,
    Failed,
}
```

No boolean soup such as:

```rust
is_loading
has_error
has_permission
location_available
```

Use enums.

---

# 17. Roadmap

## Phase 0 — GPUI Windows proof

Goal:

**Prove our chosen GPUI revision builds correctly on Windows before writing map code.**

Deliver:

```text
GPUI window
dark/light background
button
mouse events
canvas primitive
local PNG
remote PNG
```

Also record:

```text
Rust version
GPUI revision/version
Windows SDK version
target triple
```

Pin these.

### Exit condition

```text
cargo run
```

opens reliably on Windows 10/11.

Do not proceed until this works.

---

# Phase 1 — Map-core mathematics

No network. No GPUI dependency.

Implement:

```text
GeoPoint
TileCoordinate
MapCamera
Viewport
lon → tile X
lat → tile Y
tile → world pixels
geo → screen
screen → geo
visible tile calculation
```

Unit-test the math heavily.

### Exit condition

Given:

```text
camera
zoom
800×600 viewport
```

the engine deterministically returns the correct visible tile set.

---

# Phase 2 — Static GPUI map

Implement `MapView`.

Use locally bundled test tiles first.

Features:

```text
tile grid
camera center
zoom
correct tile placement
clipping
debug boundaries
```

Do not add GPS yet.

### Exit condition

A local tiled map appears without networking.

---

# Phase 3 — Network tile pipeline

Implement:

```text
TileProvider
TileClient
TileScheduler
MemoryTileCache
DiskTileCache
```

Flow:

```text
map asks for tile
↓
cache lookup
↓
async HTTP if needed
↓
decode
↓
notify GPUI
↓
repaint
```

Display attribution permanently in MapView.

### Exit condition

You can pan around an online map without blocking the UI.

---

# Phase 4 — Map interaction

Implement:

### Mouse drag

```text
mouse down
↓
record start
↓
mouse move
↓
camera delta
↓
mouse up
```

### Wheel

```text
wheel up   → zoom in
wheel down → zoom out
```

Important behavior:

**zoom toward the cursor**, not just screen center.

Also add:

```text
+ button
- button
reset north
```

### Exit condition

The application behaves like a normal desktop map.

---

# Phase 5 — Windows location

Implement:

```text
WindowsLocationSource
permission flow
one-shot fix
continuous updates
location status
accuracy
```

UI:

```text
◎ Locate me
```

Map overlay:

```text
      accuracy
    ┌──────────┐
    │    ●     │
    └──────────┘
```

The dot is the position.

Circle is horizontal accuracy.

### Exit condition

Clicking **Locate me** obtains a Windows location and centers the map.

---

# Phase 6 — Follow mode

Add:

```rust
pub enum FollowMode {
    Off,
    Follow,
}
```

Behavior:

```text
new GPS fix
   ↓
Follow?
 ├─ yes → camera follows
 └─ no  → marker only
```

Manual dragging automatically changes:

```text
Follow → Off
```

Add status information:

```text
Lat
Lon
Accuracy
Location source
Last update
```

---

# Phase 7 — Map overlays

Implement a generic overlay model:

```rust
pub enum MapFeature {
    Marker(Marker),
    Polyline(Polyline),
    Polygon(Polygon),
}
```

Then add:

```text
markers
route line
GPS history
accuracy radius
selection
```

Do drawing in the overlay canvas rather than modifying map tiles.

---

# Phase 8 — NMEA GPS

Only after Windows location is stable.

Implement:

```text
available COM ports
GPS device selection
baud-rate setting
NMEA line reader
GGA
RMC
GSA optional
VTG optional
```

Translate everything into:

```rust
LocationFix
```

Never send raw NMEA into UI code.

---

# Phase 9 — Production hardening

Add:

```text
logging
diagnostics
config file
cache limits
HTTP timeout
retry/backoff
network offline state
tile provider configuration
privacy screen
Windows packaging
installer
crash reporting strategy
```

---

# Phase 10 — Vector maps

**Only now** consider vector tiles.

Potential architecture:

```text
MVT/PBF
  ↓
decode
  ↓
map feature model
  ↓
style evaluation
  ↓
geometry
  ↓
GPUI custom renderer
```

Vector rendering gives you:

```text
smooth fractional zoom
custom styles
rotated labels
roads/polygons
better high-DPI rendering
3D later
```

But raster tiles are far more sensible for proving GPUI + Windows location first.

---

# 18. LLM coding rules

Put this into `docs/llm-rules.md`.

## Rule 1 — one phase at a time

An LLM must not implement:

```text
GPS
routing
search
vector maps
offline maps
```

while working on basic tile rendering.

---

## Rule 2 — preserve dependency direction

Allowed:

```text
ui → map-core
ui → location
ui → map-tiles

map-tiles → map-core
location → map-core
```

Forbidden:

```text
map-core → GPUI
map-core → Windows
map-core → HTTP
location → GPUI
map-tiles → UI
```

`map-core` must remain pure Rust.

---

## Rule 3 — platform code stays isolated

Any code importing:

```rust
windows::...
```

belongs under:

```text
location/windows.rs
```

or another explicitly Windows-specific platform module.

---

## Rule 4 — never block GPUI render thread

Forbidden:

```text
HTTP inside render()
disk IO inside render()
GPS polling inside render()
sleep()
blocking channel receive()
```

`render()` reads state and generates elements.

That's it.

---

## Rule 5 — errors are typed

Prefer:

```rust
enum LocationError
enum TileError
enum CacheError
```

instead of propagating arbitrary strings.

Use `thiserror` for library errors.

`anyhow` is acceptable around the executable/startup boundary.

---

## Rule 6 — no unnecessary `unwrap()`

Allowed:

```text
tests
compile-time invariants
truly impossible internal states with explanation
```

Forbidden:

```rust
http_result.unwrap()
gps_result.unwrap()
file.read().unwrap()
permission.unwrap()
```

---

# 19. Rust style

Use:

```text
rustfmt
clippy
```

Before considering a task finished:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

Naming:

```rust
GeoPoint
MapCamera
TileCoordinate
TileProvider

calculate_visible_tiles()
screen_to_geo()
geo_to_screen()
request_location()
```

Avoid abbreviations like:

```rust
mgr
loc
coord_mgr
mp
ctx2
```

unless they are universally obvious.

---

# 20. Function design rule

Bad:

```rust
fn process_map(
    x: f64,
    y: f64,
    z: i32,
    w: f32,
    h: f32,
    ...
)
```

Good:

```rust
fn visible_tiles(
    camera: &MapCamera,
    viewport: Viewport,
) -> Vec<TileCoordinate>
```

Prefer domain objects.

---

# 21. Concurrency rule

Think of tasks as producers and application state as consumer.

```text
       HTTP worker
           │
           ▼
      TileResult
           │
           ▼
      GPUI Entity
           │
           ▼
        repaint
```

and:

```text
Windows Geolocator
        │
        ▼
   LocationFix
        │
        ▼
 Location Entity
        │
        ▼
     MapView
```

Never give a background worker ownership of MapView.

---

# 22. Testing strategy

## `map-core`

Heavy unit testing.

Test:

```text
longitude -180
longitude 0
longitude +180

latitude near Mercator limits

zoom 0
zoom 1
zoom 19

tile boundaries

geo → screen → geo round-trip

camera pan

visible tile set
```

These tests need no GPUI.

---

## Tile service

Mock HTTP.

Test:

```text
200 tile
404 tile
500 response
timeout
invalid PNG
disk cache hit
memory cache hit
expired cache
duplicate concurrent request
```

---

## Location

Provide:

```rust
MockLocationSource
```

Example:

```rust
MockLocationSource::fixed(
    GeoPoint::new(-12.0464, -77.0428)
)
```

Then the UI can be developed without depending on real GPS hardware.

---

# 23. Performance targets

Treat these as engineering targets, not hard requirements:

| Operation                |                        Target |
| ------------------------ | ----------------------------: |
| pan rendering            |                       ~60 FPS |
| UI interaction           | <16 ms main-thread work/frame |
| camera math              |                         <1 ms |
| tile cache lookup        |                  <1 ms memory |
| active downloads         |                  ≤8 initially |
| visible tile calculation |     effectively instantaneous |
| GPS UI update            |           <100 ms after event |
| startup                  |                   <2 s target |

The map should remain responsive even if every network request is failing.

---

# 24. First UI

For MVP:

```text
┌──────────────────────────────────────────────────────────┐
│ GPUI Map                    [−] [+] [◎ Locate me]        │
├──────────────────────────────────────────────────────────┤
│                                                          │
│                                                          │
│                         MAP                              │
│                                                          │
│                       ◉ location                         │
│                                                          │
│                                                          │
│                                                          │
│ © OpenStreetMap contributors                            │
├──────────────────────────────────────────────────────────┤
│ Lat -12.0464  Lon -77.0428 | Accuracy 20 m | Zoom 14    │
└──────────────────────────────────────────────────────────┘
```

Don't create sidebars, accounts, routing panels, search systems, etc. yet.

---

# 25. Dependency policy

At the beginning, keep it small.

Conceptually:

```toml
gpui
windows
thiserror
serde
serde_json
tracing
```

plus whatever HTTP implementation we choose for tiles.

For GPUI specifically, **pin the exact version or commit that passes Phase 0 on Windows**. Don't let LLMs randomly upgrade GPUI because its own documentation still describes it as pre-1.0 with breaking changes. ([GitHub][1])

The current GPUI API includes `Render`, `Image`, `img`, custom `canvas`, painting APIs and Windows-specific platform code, so we don't need to introduce a second UI/rendering framework for the MVP. ([Docs.rs][6])

---

# 26. LLM task format

Every coding task given to an LLM should look approximately like this:

```text
TASK:
Implement Phase 2.3: visible raster tile rendering.

SCOPE:
- crates/map-core
- crates/ui/src/map_view.rs

DO:
- calculate visible tiles
- position existing decoded tile images
- clip map to viewport
- add debug tile borders

DO NOT:
- add networking
- add GPS
- add disk caching
- add vector tiles
- change workspace architecture
- upgrade GPUI

REQUIREMENTS:
- map-core cannot depend on GPUI
- no blocking operations in render()
- no unwrap() on runtime data
- add unit tests for map math

DONE WHEN:
- bundled local tiles render correctly
- pan changes visible tile positions
- cargo fmt passes
- cargo clippy passes
- cargo test passes

OUTPUT:
1. summary
2. files changed
3. architectural decisions
4. tests added
5. remaining issues
```

This format will dramatically improve results when several LLM sessions work on the same repository.

---

# 27. Commit policy for LLMs

One conceptual change per commit:

```text
feat(map-core): add Web Mercator projection

feat(map): calculate visible XYZ tiles

feat(ui): add GPUI map viewport

feat(tiles): add asynchronous tile loader

feat(location): add Windows geolocator

feat(map): add current-location overlay
```

Not:

```text
update stuff
fix map
changes
```

---

# 28. Milestone sequence

I would lock the development order to:

```text
M0
GPUI works on Windows
        ↓
M1
pure map mathematics
        ↓
M2
local raster tiles render
        ↓
M3
online raster tiles
        ↓
M4
pan + zoom
        ↓
M5
Windows Geolocator
        ↓
M6
location marker + follow
        ↓
M7
markers + polylines
        ↓
M8
external NMEA GPS
        ↓
M9
production packaging
        ↓
M10
vector maps / other platforms
```

The most important architectural decision is **not trying to solve “maps” with one giant GPUI component**. GPUI should only be the presentation shell. Projection, tile selection, caching and location should remain independent modules. That gives us a realistic path from a small Windows raster-map prototype to a serious native GIS/location application without throwing away the first implementation.

[1]: https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md?utm_source=chatgpt.com "zed/crates/gpui/README.md at main · zed-industries/zed · GitHub"
[2]: https://github.com/zed-industries/zed/blob/main/crates/gpui/src/elements/img.rs?utm_source=chatgpt.com "zed/crates/gpui/src/elements/img.rs at main · zed-industries/zed · GitHub"
[3]: https://github.com/zed-industries/create-gpui-app?utm_source=chatgpt.com "GitHub - zed-industries/create-gpui-app: CRA-style tool for creating new gpui apps · GitHub"
[4]: https://docs.rs/gpui/latest/gpui/struct.Image.html?utm_source=chatgpt.com "Image in gpui - Rust"
[5]: https://operations.osmfoundation.org/policies/tiles/?utm_source=chatgpt.com "Tile Usage Policy"
[6]: https://docs.rs/gpui/latest/gpui/trait.Render.html?utm_source=chatgpt.com "Render in gpui - Rust"
[7]: https://docs.rs/gpui/latest/gpui/fn.canvas.html?utm_source=chatgpt.com "canvas in gpui - Rust"
[8]: https://learn.microsoft.com/en-us/windows/win32/locationapi/windows-location-api-portal?utm_source=chatgpt.com "Location API - Win32 apps | Microsoft Learn"
[9]: https://learn.microsoft.com/en-us/uwp/api/Windows.Devices.Geolocation.Geolocator?redirectedfrom=MSDN&view=winrt-22621&utm_source=chatgpt.com "Geolocator Class (Windows.Devices.Geolocation) - Windows apps | Microsoft Learn"
[10]: https://learn.microsoft.com/en-us/uwp/api/windows.devices.geolocation.geolocator.requestaccessasync?view=winrt-26100&utm_source=chatgpt.com "Geolocator.RequestAccessAsync Method (Windows.Devices.Geolocation) - Windows apps | Microsoft Learn"
[11]: https://learn.microsoft.com/en-us/windows/apps/develop/maps-and-location/get-location?utm_source=chatgpt.com "Get the user's location - Windows apps | Microsoft Learn"
