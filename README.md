# gpuimap

`gpuimap` is a Windows-first native raster map application built with Rust and GPUI.
The workspace follows the implementation order in [plan.md](plan.md): pure Web Mercator map mathematics first, then tile I/O, location backends, and the GPUI presentation shell.

## Current implementation

- `gpuimap-map-core` contains typed coordinates, Web Mercator projection, camera math, screen/geo conversions, and visible XYZ tile calculation.
- `gpuimap-storage` owns platform-appropriate cache locations.
- `gpuimap-map-tiles` contains provider, HTTP, memory/disk cache, and worker-scheduler abstractions.
- `gpuimap-location` contains the platform-neutral location domain and a deterministic mock source. The Windows WinRT source is isolated behind `location::windows`.
- `gpuimap-ui` and `gpuimap` provide the GPUI window, toolbar, map interaction, tile presentation, attribution, and location status surface.

## Build and test

The project is pinned to Rust 1.97.1 and GPUI 0.2.2 in the manifests. On Windows 10/11 x86_64:

```text
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features
cargo run -p gpuimap
```

The first dependency fetch requires access to crates.io. The default tile provider is OpenStreetMap's standard raster service; the app includes attribution and caches tiles locally. Use a provider that permits your production workload before distributing or prefetching tiles.

## Architecture boundary

`gpuimap-map-core` is deliberately dependency-free and must not import GPUI, Windows APIs, HTTP clients, or storage. `gpuimap-location` owns location sources, while `gpuimap-map-tiles` owns tile I/O. The UI consumes domain values and never gives a background worker ownership of a GPUI view.

