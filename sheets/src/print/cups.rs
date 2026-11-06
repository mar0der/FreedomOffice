//! Printing on Linux/macOS through CUPS: pages are rendered to PDF (the same renderer
//! as "Export to PDF") and handed to `lp`. Printers are listed with `lpstat`.
//! Exposes the same API as the Windows GDI backend so the print UI is shared.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// Unused on CUPS (Windows keeps raw DEVMODE bytes here).
    pub devmode: Option<Vec<u8>>,
    pub doc_name: String,
    /// Write the PDF to a file instead of printing.
    pub output_file: Option<PathBuf>,
}

fn lpstat(args: &[&str]) -> String {
    Command::new("lpstat")
        .args(args)
        .env("LC_ALL", "C")
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Parses `lpstat -p` and `lpstat -d` output (C locale).
fn parse_printers(list: &str, default: &str) -> Vec<PrinterInfo> {
    let default = default.split_once(':').map(|(_, d)| d.trim()).filter(|d| !d.is_empty());
    let mut out: Vec<PrinterInfo> = Vec::new();
    for line in list.lines() {
        let Some(rest) = line.strip_prefix("printer ") else { continue };
        let Some(name) = rest.split_whitespace().next() else { continue };
        let status = if rest.contains("disabled") {
            "Offline"
        } else if rest.contains("now printing") {
            "Printing"
        } else {
            "Ready"
        };
        out.push(PrinterInfo { name: name.to_string(), is_default: default == Some(name), status: status.into() });
    }
    out.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// Printers known to CUPS.
pub fn printers() -> Vec<PrinterInfo> {
    parse_printers(&lpstat(&["-p"]), &lpstat(&["-d"]))
}

/// CUPS has no driver dialog to open from here.
pub fn printer_properties(_hwnd: isize, _printer: &str, _devmode: &mut Option<Vec<u8>>) -> bool {
    false
}

/// Sends the pages to the printer as a PDF job.
pub fn print(pages: &[Page], settings: &PrintSettings) -> Result<(), String> {
    if pages.is_empty() {
        return Err("There's nothing to print.".into());
    }
    let pdf = super::pdf::render(pages);
    if let Some(path) = &settings.output_file {
        return std::fs::write(path, &pdf).map_err(|e| format!("Could not write {}: {e}", path.display()));
    }
    let sides = match settings.duplex {
        Duplex::OneSided => "sides=one-sided",
        Duplex::LongEdge => "sides=two-sided-long-edge",
        Duplex::ShortEdge => "sides=two-sided-short-edge",
    };
    let collate = if settings.collate { "collate=true" } else { "collate=false" };
    let copies = settings.copies.max(1).to_string();
    let mut child = Command::new("lp")
        .args(["-d", &settings.printer, "-n", &copies, "-t", &settings.doc_name, "-o", sides, "-o", collate, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run lp (is CUPS installed?): {e}"))?;
    child.stdin.take().ok_or("lp has no input")?.write_all(&pdf).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("Printing failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lpstat() {
        let list = "printer Office is idle.  enabled since Thu\nprinter Attic disabled since Wed -\n\treason unknown\n";
        let p = parse_printers(list, "system default destination: Office\n");
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].name, "Office");
        assert!(p[0].is_default);
        assert_eq!(p[0].status, "Ready");
        assert_eq!(p[1].status, "Offline");
        assert!(parse_printers("", "no system default destination").is_empty());
    }
}
