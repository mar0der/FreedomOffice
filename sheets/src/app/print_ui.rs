//! File > Print: printer, settings and live preview (work in progress).

use eframe::egui;

use super::App;

#[derive(Default)]
pub struct PrintState {}

impl App {
    pub fn print_page_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Print");
    }
}
