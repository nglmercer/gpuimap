# Map engine

The map engine uses north-up Web Mercator (EPSG:3857) and standard XYZ raster tiles. Longitudes wrap at the antimeridian, while latitude is clamped to the Web Mercator limit of ±85.05112878 degrees. The camera retains fractional zoom, and tile requests use the integer floor of that zoom so raster tiles scale smoothly.

`Viewport::visible_tiles` returns deterministic tile coordinates for a camera and viewport. `MapCamera::screen_to_geo` and `MapCamera::geo_to_screen` share the same normalized world coordinate model, which makes cursor-centered zoom and round-trip testing possible without a renderer.

