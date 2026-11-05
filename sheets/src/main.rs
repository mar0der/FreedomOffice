//! FreedomOffice Sheets: a lightweight, open-source spreadsheet application.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod chart;
mod cloud;
mod condfmt;
mod datatools;
mod filter;
mod formula;
mod icon;
mod iconset;
mod io;
mod model;
mod numfmt;
mod page;
mod print;
mod solver;
mod sparkline;
mod table;
mod validation;
mod whatif;

use eframe::egui;

fn main() -> eframe::Result<()> {
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
            .with_icon(std::sync::Arc::new(icon)),
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
