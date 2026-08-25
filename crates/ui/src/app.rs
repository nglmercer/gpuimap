use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};
use location::LocationSource;
use map_tiles::{DiskTileCache, OpenStreetMapProvider, TileProvider, TileScheduler};
use storage::CachePaths;

use crate::map_view::MapView;
use crate::toolbar::MainWindow;

const USER_AGENT: &str = "gpuimap/0.1 (native Windows map application)";

/// Starts the native GPUI application.
pub fn run() {
    let provider = OpenStreetMapProvider::default();
    let disk_cache = match CachePaths::for_app("gpuimap") {
        Ok(paths) => match DiskTileCache::new(&paths, provider.cache_namespace()) {
            Ok(cache) => Some(cache),
            Err(error) => {
                eprintln!("tile disk cache disabled: {error}");
                None
            }
        },
        Err(error) => {
            eprintln!("tile disk cache disabled: {error}");
            None
        }
    };

    let scheduler = match TileScheduler::new_http(provider, USER_AGENT, 256, disk_cache, 8) {
        Ok(scheduler) => scheduler,
        Err(error) => {
            eprintln!("cannot initialize tile client: {error}");
            return;
        }
    };

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1_100.0), px(760.0)), cx);
        let scheduler = scheduler;
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_window, cx| {
                cx.new(|cx| {
                    let source = default_location_source();
                    let map = cx.new(|cx| MapView::new(scheduler, source, cx));
                    MainWindow::new(map)
                })
            },
        );
        if let Err(error) = result {
            eprintln!("could not open gpuimap window: {error}");
            cx.quit();
        } else {
            cx.activate(true);
        }
    });
}

fn default_location_source() -> Box<dyn LocationSource> {
    #[cfg(windows)]
    {
        Box::new(location::windows::WindowsLocationSource::new())
    }

    #[cfg(not(windows))]
    {
        Box::new(location::MockLocationSource::fixed(
            map_core::GeoPoint::new(-12.0464, -77.0428),
        ))
    }
}
