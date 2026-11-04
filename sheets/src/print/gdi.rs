//! Printing to Windows printers through GDI (to be implemented).

use std::path::PathBuf;

use super::Page;

#[derive(Clone, Debug, PartialEq)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
    /// Short status text ("Ready", "Offline", ...).
    pub status: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplex {
    OneSided,
    LongEdge,
    ShortEdge,
}

#[derive(Clone, Debug)]
pub struct PrintSettings {
    pub printer: String,
    pub copies: u16,
    pub collate: bool,
    pub duplex: Duplex,
    /// Driver settings captured from "Printer Properties" (raw DEVMODEW bytes).
    pub devmode: Option<Vec<u8>>,
    pub doc_name: String,
    /// Print to a file instead (used for testing with "Microsoft Print to PDF").
    pub output_file: Option<PathBuf>,
}

/// Installed printers (local and network connections).
pub fn printers() -> Vec<PrinterInfo> {
    Vec::new()
}

/// Opens the driver's own properties dialog; updates `devmode` when the user clicks OK.
pub fn printer_properties(_hwnd: isize, _printer: &str, _devmode: &mut Option<Vec<u8>>) -> bool {
    false
}

/// Sends the pages to the printer.
pub fn print(_pages: &[Page], _settings: &PrintSettings) -> Result<(), String> {
    Err("Printing is not available yet.".into())
}
