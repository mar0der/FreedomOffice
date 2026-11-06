//! FreedomOffice Sheets: a lightweight, open-source spreadsheet application.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod chart;
mod cloud;
mod condfmt;
mod datatools;
mod filter;
mod formula;
mod hash;
mod icon;
mod iconset;
mod io;
mod live;
mod model;
mod numfmt;
mod page;
mod pivot;
mod print;
mod solver;
mod sparkline;
mod structure;
mod table;
mod validation;
mod whatif;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Windows links a 256 MB main-thread stack (build.rs). Elsewhere the main thread keeps
    // the OS default (8 MB on Linux), so run the UI on a thread with the same big stack.
    #[cfg(target_os = "linux")]
    {
        let ui = std::thread::Builder::new()
            .name("ui".into())
            .stack_size(256 << 20)
            .spawn(|| run().map_err(|e| e.to_string()))
            .expect("spawn UI thread");
        if let Err(e) = ui.join().unwrap_or_else(|e| std::panic::resume_unwind(e)) {
            eprintln!("FreedomSheets: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    #[allow(unreachable_code)]
    run()
}

fn run() -> eframe::Result<()> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // "--print-preview" opens File > Print right away (like a print-preview shortcut).
    let preview = args.iter().any(|a| a == "--print-preview");
    let path = args.iter().find(|a| !a.to_string_lossy().starts_with("--")).map(std::path::PathBuf::from);
    let icon = egui::IconData { rgba: icon::icon_rgba(64), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FreedomOffice Sheets")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(std::sync::Arc::new(icon))
            .with_app_id("freedom-sheets"),
        #[cfg(target_os = "linux")]
        event_loop_builder: Some(Box::new(|b| {
            use winit::platform::{wayland::EventLoopBuilderExtWayland, x11::EventLoopBuilderExtX11};
            EventLoopBuilderExtX11::with_any_thread(b, true);
            EventLoopBuilderExtWayland::with_any_thread(b, true);
        })),
        ..Default::default()
    };
    eframe::run_native("FreedomOffice Sheets", options, Box::new(move |cc| {
            let mut app = app::App::new(cc, path);
            if preview {
                app.backstage = Some(app::Backstage::Print);
            }
            Ok(Box::new(app))
        }))
}
