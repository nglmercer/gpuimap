use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc},
    time::Duration,
};

use gpui::{
    Context, Image, ImageFormat, MouseButton, Render, ScrollWheelEvent, Window, div, img,
    prelude::*, px, rgb, rgba,
};
use location::{
    LocationError, LocationEvent, LocationFix, LocationSink, LocationSource, LocationState,
    PermissionStatus,
};
use map_core::{DEFAULT_ZOOM, GeoPoint, MapCamera, ScreenPoint, TileCoordinate, Viewport};
use map_tiles::{OpenStreetMapProvider, TilePriority, TileProvider, TileResult, TileScheduler};

use crate::toolbar;

pub const MAP_TOP_OFFSET: f32 = toolbar::TOOLBAR_HEIGHT;
const STATUS_BAR_HEIGHT: f32 = 28.0;
const DEFAULT_LATITUDE: f64 = -12.0464;
const DEFAULT_LONGITUDE: f64 = -77.0428;

/// Whether new fixes recenter the camera.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FollowMode {
    Off,
    Follow,
}

pub struct MapView {
    pub camera: MapCamera,
    pub viewport: Viewport,
    pub location: Option<LocationFix>,
    pub location_state: LocationState,
    pub follow_mode: FollowMode,
    scheduler: TileScheduler<OpenStreetMapProvider>,
    source: Box<dyn LocationSource>,
    location_sink: LocationSink,
    location_rx: mpsc::Receiver<LocationEvent>,
    images: HashMap<TileCoordinate, Arc<Image>>,
    requested: HashSet<TileCoordinate>,
    failed: HashSet<TileCoordinate>,
    last_tile_error: Option<String>,
    last_drag_position: Option<ScreenPoint>,
}

impl MapView {
    pub fn new(
        scheduler: TileScheduler<OpenStreetMapProvider>,
        source: Box<dyn LocationSource>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (location_tx, location_rx) = mpsc::channel();
        let location_sink: LocationSink = Arc::new(move |event| {
            let _ = location_tx.send(event);
        });
        let mut view = Self {
            camera: MapCamera::new(
                GeoPoint::new(DEFAULT_LATITUDE, DEFAULT_LONGITUDE),
                DEFAULT_ZOOM,
            ),
            viewport: Viewport::new(1_100.0, 688.0).unwrap_or(Viewport {
                width: 1_100.0,
                height: 688.0,
            }),
            location: None,
            location_state: LocationState::Disabled,
            follow_mode: FollowMode::Off,
            scheduler,
            source,
            location_sink,
            location_rx,
            images: HashMap::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
            last_tile_error: None,
            last_drag_position: None,
        };
        view.request_visible_tiles();

        // Polling is non-blocking: tile workers and platform callbacks do all
        // I/O elsewhere, while this short foreground task only transfers ready
        // results into GPUI-owned state.
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if view
                    .update(cx, |view, cx| view.consume_background_results(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        view
    }

    pub fn zoom_by(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.camera.zoom_by(delta);
        self.request_visible_tiles();
        cx.notify();
    }

    pub fn reset_view(&mut self, cx: &mut Context<Self>) {
        self.camera = MapCamera::new(
            GeoPoint::new(DEFAULT_LATITUDE, DEFAULT_LONGITUDE),
            DEFAULT_ZOOM,
        );
        self.follow_mode = FollowMode::Off;
        self.request_visible_tiles();
        cx.notify();
    }

    pub fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        self.follow_mode = match self.follow_mode {
            FollowMode::Off => FollowMode::Follow,
            FollowMode::Follow => FollowMode::Off,
        };
        if self.follow_mode == FollowMode::Follow
            && let Some(fix) = &self.location
        {
            self.camera.set_center(fix.position);
            self.request_visible_tiles();
        }
        cx.notify();
    }

    /// Performs the explicit foreground permission flow, then gets a first
    /// fix and starts continuous updates. The Windows adapter keeps its
    /// `RequestAccessAsync` call on this UI action path as required by WinRT.
    pub fn locate_me(&mut self, cx: &mut Context<Self>) {
        self.location_state = LocationState::RequestingPermission;
        match self.source.request_permission() {
            Ok(PermissionStatus::Allowed) => {
                self.location_state = LocationState::Searching;
                if let Err(error) = self.source.start_updates(Arc::clone(&self.location_sink)) {
                    self.location_state = LocationState::Unavailable(error);
                }
                match self.source.current_position() {
                    Ok(fix) => self.center_on_fix(fix),
                    Err(error) => self.location_state = LocationState::Unavailable(error),
                }
            }
            Ok(PermissionStatus::Denied) => {
                self.location_state = LocationState::PermissionDenied;
            }
            Ok(PermissionStatus::Unspecified) => {
                self.location_state = LocationState::Unavailable(LocationError::Unavailable);
            }
            Err(error) => {
                self.location_state = LocationState::Unavailable(error);
            }
        }
        cx.notify();
    }

    pub fn location_state_label(&self) -> String {
        match &self.location_state {
            LocationState::Disabled => "Location disabled".into(),
            LocationState::RequestingPermission => "Requesting permission".into(),
            LocationState::PermissionDenied => "Permission denied".into(),
            LocationState::Searching => "Searching".into(),
            LocationState::Available(_) => format!("{}", self.source.backend()),
            LocationState::Unavailable(error) => format!("Unavailable: {error}"),
        }
    }

    pub fn loaded_tile_count(&self) -> usize {
        self.images.len()
    }

    fn consume_background_results(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Some(TileResult { tile, result, .. }) = self.scheduler.try_recv() {
            self.requested.remove(&tile);
            match result {
                Ok(data) => {
                    let format = data
                        .content_type
                        .as_deref()
                        .and_then(ImageFormat::from_mime_type)
                        .unwrap_or(ImageFormat::Png);
                    self.images.insert(
                        tile,
                        Arc::new(Image::from_bytes(format, data.bytes().to_vec())),
                    );
                    self.failed.remove(&tile);
                }
                Err(error) => {
                    self.failed.insert(tile);
                    self.last_tile_error = Some(error.to_string());
                }
            }
            changed = true;
        }

        while let Ok(event) = self.location_rx.try_recv() {
            self.apply_location_event(event);
            changed = true;
        }

        if changed {
            cx.notify();
        }
    }

    fn apply_location_event(&mut self, event: LocationEvent) {
        match event {
            LocationEvent::Fix(fix) => self.apply_fix(fix),
            LocationEvent::State(state) => self.location_state = state,
        }
    }

    fn apply_fix(&mut self, fix: LocationFix) {
        self.location_state = LocationState::Available(fix.clone());
        self.location = Some(fix.clone());
        if self.follow_mode == FollowMode::Follow {
            self.camera.set_center(fix.position);
            self.request_visible_tiles();
        }
    }

    fn center_on_fix(&mut self, fix: LocationFix) {
        let position = fix.position;
        self.apply_fix(fix);
        self.camera.set_center(position);
        self.request_visible_tiles();
    }

    fn request_visible_tiles(&mut self) {
        for tile in self.camera.visible_tiles(self.viewport) {
            if self.images.contains_key(&tile)
                || self.requested.contains(&tile)
                || self.failed.contains(&tile)
            {
                continue;
            }
            if self.scheduler.request(tile, TilePriority::VISIBLE) {
                self.requested.insert(tile);
            }
        }
    }

    fn local_cursor(&self, point: gpui::Point<gpui::Pixels>) -> ScreenPoint {
        ScreenPoint {
            x: f32::from(point.x),
            y: f32::from(point.y) - MAP_TOP_OFFSET,
        }
    }

    fn render_tile(&self, tile: TileCoordinate) -> gpui::AnyElement {
        let placement = self.viewport.tile_placement(&self.camera, tile);
        let tile_id = (u64::from(tile.zoom) << 56) | (u64::from(tile.x) << 28) | u64::from(tile.y);
        let mut tile_element = div()
            .id(("tile", tile_id))
            .absolute()
            .left(px(placement.origin.x))
            .top(px(placement.origin.y))
            .w(px(placement.size))
            .h(px(placement.size));

        if let Some(image) = self.images.get(&tile) {
            tile_element = tile_element.child(img(image.clone()).size_full());
        } else {
            tile_element = tile_element
                .bg(rgb(0x243447))
                .border_1()
                .border_color(rgb(0x526579))
                .text_color(rgb(0xb7c9d6))
                .text_xs()
                .items_center()
                .justify_center()
                .child(format!("{}/{}/{}", tile.zoom, tile.x, tile.y));
        }
        tile_element.into_any_element()
    }

    fn render_location_overlay(&self) -> Vec<gpui::AnyElement> {
        let Some(fix) = &self.location else {
            return Vec::new();
        };
        let point = self.camera.geo_to_screen(fix.position, self.viewport);
        let mut elements = Vec::new();
        if let Some(accuracy) = fix.horizontal_accuracy_m
            && accuracy > 0.0
        {
            let meters_per_pixel = 156_543.033_92 * fix.position.latitude.to_radians().cos()
                / 2.0_f64.powf(self.camera.zoom);
            let radius = (accuracy / meters_per_pixel.max(0.01)) as f32;
            elements.push(
                div()
                    .absolute()
                    .left(px(point.x - radius))
                    .top(px(point.y - radius))
                    .w(px(radius * 2.0))
                    .h(px(radius * 2.0))
                    .rounded_full()
                    .border_1()
                    .border_color(rgba(0x60a5fa66))
                    .bg(rgba(0x60a5fa18))
                    .into_any_element(),
            );
        }
        elements.push(
            div()
                .absolute()
                .left(px(point.x - 6.0))
                .top(px(point.y - 6.0))
                .w(px(12.0))
                .h(px(12.0))
                .rounded_full()
                .border_2()
                .border_color(rgb(0xffffff))
                .bg(rgb(0x2563eb))
                .into_any_element(),
        );
        elements
    }
}

impl Render for MapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_size = window.viewport_size();
        let width = f32::from(window_size.width).max(1.0);
        let height = (f32::from(window_size.height) - MAP_TOP_OFFSET - STATUS_BAR_HEIGHT).max(1.0);
        if let Ok(viewport) = Viewport::new(width, height)
            && viewport != self.viewport
        {
            self.viewport = viewport;
            self.request_visible_tiles();
        }

        let tiles = self
            .camera
            .visible_tiles(self.viewport)
            .into_iter()
            .map(|tile| self.render_tile(tile));
        let overlays = self.render_location_overlay();
        let attribution_text = self.scheduler.provider().attribution().to_owned();
        let attribution = div()
            .absolute()
            .left(px(8.0))
            .bottom(px(8.0))
            .px_2()
            .py_1()
            .bg(rgba(0x111827cc))
            .text_color(rgb(0xe5e7eb))
            .text_xs()
            .child(attribution_text);

        let error = self.last_tile_error.as_ref().map(|error| {
            div()
                .absolute()
                .right(px(8.0))
                .bottom(px(8.0))
                .px_2()
                .py_1()
                .bg(rgba(0x7f1d1dcc))
                .text_color(rgb(0xfecaca))
                .text_xs()
                .child(error.clone())
                .into_any_element()
        });

        let mut root = div()
            .id("map-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(0x17212b))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.follow_mode = FollowMode::Off;
                    this.last_drag_position = Some(this.local_cursor(event.position));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if !event.dragging() {
                    return;
                }
                let current = this.local_cursor(event.position);
                if let Some(previous) = this.last_drag_position.replace(current) {
                    this.camera.pan_by_screen_delta(
                        ScreenPoint {
                            x: current.x - previous.x,
                            y: current.y - previous.y,
                        },
                        this.viewport,
                    );
                    this.request_visible_tiles();
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.last_drag_position = None;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.last_drag_position = None;
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(36.0)).y;
                let cursor = this.local_cursor(event.position);
                this.camera.zoom_towards_screen(
                    this.camera.zoom + f64::from(delta) / 144.0,
                    cursor,
                    this.viewport,
                );
                this.request_visible_tiles();
                cx.notify();
            }))
            .children(tiles)
            .children(overlays)
            .child(attribution);
        if let Some(error) = error {
            root = root.child(error);
        }
        root
    }
}
