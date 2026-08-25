use gpui::{App, Context, Entity, MouseButton, Render, Window, div, prelude::*, px, rgb};

use crate::map_view::MapView;
use crate::status_bar;

pub(crate) const TOOLBAR_HEIGHT: f32 = 44.0;

pub struct MainWindow {
    map: Entity<MapView>,
}

impl MainWindow {
    pub fn new(map: Entity<MapView>) -> Self {
        Self { map }
    }
}

impl Render for MainWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let map = self.map.clone();
        let status = status_bar::render(self.map.read(cx));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x111827))
            .child(render_toolbar(map.clone()))
            .child(div().flex_1().min_h(px(1.0)).child(map))
            .child(status)
    }
}

pub fn render_toolbar(map: Entity<MapView>) -> impl IntoElement {
    div()
        .id("toolbar")
        .h(px(TOOLBAR_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .bg(rgb(0x1f2937))
        .text_color(rgb(0xf9fafb))
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::BOLD)
                .child("GPUI Map"),
        )
        .child(div().flex_1())
        .child(button("−", map.clone(), |view, cx| {
            view.zoom_by(-1.0, cx);
        }))
        .child(button("+", map.clone(), |view, cx| {
            view.zoom_by(1.0, cx);
        }))
        .child(button("Reset", map.clone(), |view, cx| {
            view.reset_view(cx);
        }))
        .child(button("◎ Locate me", map.clone(), |view, cx| {
            view.locate_me(cx);
        }))
        .child(button("Location settings", map.clone(), |view, cx| {
            view.open_location_settings(cx);
        }))
        .child(button("Follow", map, |view, cx| {
            view.toggle_follow(cx);
        }))
}

fn button(
    label: &'static str,
    map: Entity<MapView>,
    action: impl Fn(&mut MapView, &mut Context<MapView>) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .px_3()
        .py_1()
        .rounded_sm()
        .bg(rgb(0x374151))
        .hover(|style| style.bg(rgb(0x4b5563)))
        .active(|style| style.bg(rgb(0x2563eb)))
        .cursor_pointer()
        .child(label)
        .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
            map.update(cx, |view, cx| action(view, cx));
        })
}
