# LLM coding rules

1. Work on one roadmap phase at a time and keep the task scope explicit.
2. Preserve dependency direction: `map-core` stays pure Rust; platform APIs stay in platform modules; tile I/O stays in `map-tiles`.
3. Never perform HTTP, disk I/O, GPS polling, sleeps, or blocking receives from GPUI `render()`.
4. Prefer typed errors and domain structs over stringly typed state or tuples.
5. Do not add routing, search, vector tiles, bulk offline downloading, or external GPS protocol parsing while basic raster map behavior is incomplete.
6. Keep GPUI pinned unless a deliberate compatibility change includes an updated build record and verification.
7. Before handing off a change, run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features`, and `cargo test --workspace`.

