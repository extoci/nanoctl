#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod app;
mod assets;
mod config;
mod labs;
mod tailnet;

use app::Planck;
use assets::EmbeddedAssets;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

fn main() {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("nanoctl-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    Application::new()
        .with_assets(EmbeddedAssets)
        .run(|cx: &mut App| {
            cx.activate(true);
            let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("nanoctl".into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(960.0), px(620.0))),
                    ..Default::default()
                },
                |_, cx| {
                    let view = cx.new(|_| Planck::new());
                    view.update(cx, |planck, cx| {
                        planck.refresh(cx);
                        planck.refresh_agent(cx);
                    });
                    view
                },
            )
            .expect("open nanoctl window");
        });
}
