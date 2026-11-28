#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod app;
mod config;
mod labs;
mod tailnet;

use app::Planck;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

fn main() {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("nanoctl-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    Application::new().run(|cx: &mut App| {
        cx.activate(true);
        let bounds = Bounds::centered(None, size(px(980.0), px(680.0)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("nanoctl".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.0), px(520.0))),
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
