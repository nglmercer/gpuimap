use gpui::{IntoElement, div, prelude::*, px, rgb};

use crate::map_view::MapView;

const STATUS_HEIGHT: f32 = 28.0;

pub fn render(view: &MapView) -> impl IntoElement {
    let location = view
        .location
        .as_ref()
        .map(|fix| {
            let accuracy = fix
                .horizontal_accuracy_m
                .map_or_else(|| "unknown".to_owned(), |value| format!("{value:.0} m"));
            format!(
                "Lat {:.5}  Lon {:.5}  | Accuracy {} | {}",
                fix.position.latitude,
                fix.position.longitude,
                accuracy,
                view.location_state_label()
            )
        })
        .unwrap_or_else(|| format!("{} | No location fix", view.location_state_label()));

    div()
        .id("status-bar")
        .h(px(STATUS_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .px_3()
        .gap_4()
        .bg(rgb(0x111827))
        .text_color(rgb(0xd1d5db))
        .text_sm()
        .child(location)
        .child(div().flex_1())
        .child(format!(
            "Zoom {:.1}  |  {} tiles",
            view.camera.zoom,
            view.loaded_tile_count()
        ))
}
