//! Printing to Windows printers through GDI.
//!
//! Pages from the layout engine (points, origin at the paper's top-left) are drawn on a
//! printer device context with the real TrueType fonts. Driver settings travel as raw
//! DEVMODEW bytes so the UI can keep what the driver's own properties dialog returned.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, COLORREF, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateDCW, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, ExtCreatePen, ExtTextOutW, FillRect,
    GetDeviceCaps, GetStockObject, GetTextExtentPoint32W, GetTextMetricsW, IntersectClipRect, LineTo, MoveToEx, Polygon,
    Polyline, ResetDCW, RestoreDC, SaveDC, SelectObject, SetBkMode, SetGraphicsMode, SetTextAlign, SetTextColor, DEVMODEW,
    DEVMODEW_0_0, HBRUSH, HDC, HFONT, HGDIOBJ, HPEN, LOGBRUSH, TEXTMETRICW,
};
use windows_sys::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW, PRINTER_HANDLE,
    PRINTER_INFO_4W,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DeviceCapabilitiesW, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};

use super::{Align, Op, Page, PRect, TextOp};
use crate::model::VAlign;

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

// ---------------------------------------------------------------------------
// Win32 constants (kept local: the windows-sys typedefs differ from the parameter types)
// ---------------------------------------------------------------------------

const PRINTER_ENUM_LOCAL: u32 = 0x2;
const PRINTER_ENUM_CONNECTIONS: u32 = 0x4;
const PRINTER_ATTRIBUTE_WORK_OFFLINE: u32 = 0x400;

const DM_OUT_BUFFER: u32 = 2;
const DM_IN_PROMPT: u32 = 4;
const DM_IN_BUFFER: u32 = 8;
const IDOK: i32 = 1;

const DM_ORIENTATION: u32 = 0x1;
const DM_PAPERSIZE: u32 = 0x2;
const DM_COPIES: u32 = 0x100;
const DM_DUPLEX: u32 = 0x1000;
const DM_COLLATE: u32 = 0x8000;
const DMORIENT_PORTRAIT: i16 = 1;
const DMORIENT_LANDSCAPE: i16 = 2;
const DMDUP_SIMPLEX: i16 = 1;
const DMDUP_VERTICAL: i16 = 2;
const DMDUP_HORIZONTAL: i16 = 3;

const DC_COPIES: u16 = 18;
const DC_COLLATE: u16 = 22;

const LOGPIXELSX: i32 = 88;
const LOGPIXELSY: i32 = 90;
const PHYSICALOFFSETX: i32 = 112;
const PHYSICALOFFSETY: i32 = 113;

const GM_ADVANCED: i32 = 2;
const TRANSPARENT: i32 = 1;
const TA_LEFT_TOP: u32 = 0;
const TA_LEFT_BASELINE: u32 = 24;
const NULL_PEN: i32 = 8;
const BLACK_PEN: i32 = 7;
const WHITE_BRUSH: i32 = 0;
const SYSTEM_FONT: i32 = 13;
const BS_SOLID: u32 = 0;
const PS_GEOMETRIC: u32 = 0x10000;
const PS_SOLID: u32 = 0;
const PS_USERSTYLE: u32 = 7;
const PS_ENDCAP_FLAT: u32 = 0x200;
const PS_JOIN_ROUND: u32 = 0;
const PS_JOIN_MITER: u32 = 0x2000;

const DT_LEFT: u32 = 0;
const DT_CENTER: u32 = 1;
const DT_RIGHT: u32 = 2;
const DT_WORDBREAK: u32 = 0x10;
const DT_NOCLIP: u32 = 0x100;
const DT_CALCRECT: u32 = 0x400;
const DT_NOPREFIX: u32 = 0x800;

const FW_NORMAL: i32 = 400;
const FW_BOLD: i32 = 700;
const DEFAULT_CHARSET: u32 = 1;
const OUT_TT_PRECIS: u32 = 4;
const CLIP_DEFAULT_PRECIS: u32 = 0;
const CLIP_LH_ANGLES: u32 = 0x10;
const ANTIALIASED_QUALITY: u32 = 4;

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_CANCELLED: u32 = 1223;
const ERROR_INVALID_PRINTER_NAME: u32 = 1801;
const ERROR_PRINTER_NOT_FOUND: u32 = 3012;

/// Cell padding used by the grid and the PDF backend, in points.
const PAD: f32 = 2.0;
/// Characters in DEVMODEW.dmDeviceName (including the terminating NUL).
const CCHDEVICENAME: usize = 32;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// 0xRRGGBB -> GDI COLORREF (0x00BBGGRR).
fn colorref(rgb: u32) -> COLORREF {
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

/// Maps layout points (from the paper's top-left) to printer device units
/// (from the printable area's top-left).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapper {
    dpi_x: f32,
    dpi_y: f32,
    off_x: f32,
    off_y: f32,
}

impl Mapper {
    fn fx(&self, pt: f32) -> f32 {
        pt * self.dpi_x / 72.0 - self.off_x
    }
    fn fy(&self, pt: f32) -> f32 {
        pt * self.dpi_y / 72.0 - self.off_y
    }
    fn x(&self, pt: f32) -> i32 {
        self.fx(pt).round() as i32
    }
    fn y(&self, pt: f32) -> i32 {
        self.fy(pt).round() as i32
    }
    /// A horizontal length in device units.
    fn dx(&self, pt: f32) -> f32 {
        pt * self.dpi_x / 72.0
    }
    /// A vertical length in device units.
    fn dy(&self, pt: f32) -> f32 {
        pt * self.dpi_y / 72.0
    }
    /// Pen width in device units (never thinner than one device pixel).
    fn pen(&self, pt: f32) -> u32 {
        (pt * self.dpi_x.max(self.dpi_y) / 72.0).round().max(1.0) as u32
    }
    fn rect(&self, r: PRect) -> RECT {
        RECT { left: self.x(r.x), top: self.y(r.y), right: self.x(r.right()), bottom: self.y(r.bottom()) }
    }
}

/// Byte offsets of the DEVMODEW fields we read or patch.
mod dm {
    use super::*;
    pub const SIZE: usize = offset_of!(DEVMODEW, dmSize);
    pub const DRIVER_EXTRA: usize = offset_of!(DEVMODEW, dmDriverExtra);
    pub const FIELDS: usize = offset_of!(DEVMODEW, dmFields);
    const U1: usize = offset_of!(DEVMODEW, Anonymous1);
    pub const ORIENTATION: usize = U1 + offset_of!(DEVMODEW_0_0, dmOrientation);
    pub const PAPER_SIZE: usize = U1 + offset_of!(DEVMODEW_0_0, dmPaperSize);
    pub const COPIES: usize = U1 + offset_of!(DEVMODEW_0_0, dmCopies);
    pub const DUPLEX: usize = offset_of!(DEVMODEW, dmDuplex);
    pub const COLLATE: usize = offset_of!(DEVMODEW, dmCollate);
}

fn rd_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn rd_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn wr_i16(b: &mut [u8], at: usize, v: i16) {
    if let Some(s) = b.get_mut(at..at + 2) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}
fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    if let Some(s) = b.get_mut(at..at + 4) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

/// True when `b` holds a complete DEVMODEW (public part plus driver data).
fn devmode_ok(b: &[u8]) -> bool {
    match (rd_u16(b, dm::SIZE), rd_u16(b, dm::DRIVER_EXTRA)) {
        (Some(size), Some(extra)) => (size as usize) > dm::COLLATE + 2 && b.len() >= size as usize + extra as usize,
        _ => false,
    }
}

/// The printer name a DEVMODEW was made for.
fn devmode_device(b: &[u8]) -> String {
    let units: Vec<u16> = (0..CCHDEVICENAME).map_while(|i| rd_u16(b, i * 2)).take_while(|u| *u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Whether a stored DEVMODEW belongs to `printer` (dmDeviceName holds at most 31 characters).
fn devmode_matches(b: &[u8], printer: &str) -> bool {
    if !devmode_ok(b) {
        return false;
    }
    let want: Vec<u16> = printer.encode_utf16().take(CCHDEVICENAME - 1).collect();
    devmode_device(b).encode_utf16().eq(want)
}

/// Settings written into the DEVMODEW for a run of pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DevPatch {
    landscape: bool,
    paper: u16,
    copies: u16,
    collate: bool,
    duplex: Duplex,
}

/// Writes the patch into a DEVMODEW buffer and sets the matching dmFields bits.
fn patch_devmode(b: &mut [u8], p: &DevPatch) -> bool {
    if !devmode_ok(b) {
        return false;
    }
    let Some(mut fields) = rd_u32(b, dm::FIELDS) else { return false };
    wr_i16(b, dm::ORIENTATION, if p.landscape { DMORIENT_LANDSCAPE } else { DMORIENT_PORTRAIT });
    wr_i16(b, dm::PAPER_SIZE, p.paper as i16);
    wr_i16(b, dm::COPIES, p.copies.clamp(1, i16::MAX as u16) as i16);
    wr_i16(b, dm::COLLATE, p.collate as i16);
    let dup = match p.duplex {
        Duplex::OneSided => DMDUP_SIMPLEX,
        Duplex::LongEdge => DMDUP_VERTICAL,
        Duplex::ShortEdge => DMDUP_HORIZONTAL,
    };
    wr_i16(b, dm::DUPLEX, dup);
    fields |= DM_ORIENTATION | DM_PAPERSIZE | DM_COPIES | DM_COLLATE | DM_DUPLEX;
    // a specific paper size replaces any custom length / width / form the driver had
    fields &= !(0x4 | 0x8 | 0x10000);
    wr_u32(b, dm::FIELDS, fields);
    true
}

/// Short text for PRINTER_INFO_2 status flags / PRINTER_INFO_4 attributes.
fn status_text(status: u32, attributes: u32) -> &'static str {
    const FLAGS: [(u32, &str); 14] = [
        (0x0000_0001, "Paused"),
        (0x0000_0004, "Deleting"),
        (0x0000_0080, "Offline"),
        (0x0000_1000, "Not available"),
        (0x0000_0008, "Paper jam"),
        (0x0000_0010, "Out of paper"),
        (0x0004_0000, "Out of toner"),
        (0x0040_0000, "Door open"),
        (0x0010_0000, "Needs attention"),
        (0x0000_0040, "Paper problem"),
        (0x0000_0002, "Error"),
        (0x0002_0000, "Toner low"),
        (0x0000_0400, "Printing"),
        (0x0000_0200, "Busy"),
    ];
    if let Some((_, s)) = FLAGS.iter().find(|(f, _)| status & f != 0) {
        return s;
    }
    if attributes & PRINTER_ATTRIBUTE_WORK_OFFLINE != 0 {
        return "Offline";
    }
    "Ready"
}

/// Default first, then alphabetical (case-insensitive).
fn sort_printers(list: &mut [PrinterInfo]) {
    list.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// Corners of a rotated text box relative to its baseline origin:
/// returns (min_x, max_x, min_y, max_y) in device units (y down).
fn rotated_bounds(width: f32, ascent: f32, descent: f32, deg: f32) -> (f32, f32, f32, f32) {
    let (s, c) = deg.to_radians().sin_cos();
    // along the baseline: (c, -s); towards the ascent: (-s, -c)
    let mut b = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    for along in [0.0, width] {
        for up in [-descent, ascent] {
            let x = along * c - up * s;
            let y = -along * s - up * c;
            b = (b.0.min(x), b.1.max(x), b.2.min(y), b.3.max(y));
        }
    }
    b
}

/// xlsx rotation -> counter-clockwise degrees (None for none / stacked).
fn rotation_degrees(rot: u8) -> Option<f32> {
    match rot {
        1..=90 => Some(rot as f32),
        91..=180 => Some(-((rot - 90) as f32)),
        _ => None,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a NUL-terminated UTF-16 string (bounded, null-safe).
///
/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 string.
unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    // SAFETY: the caller guarantees a terminated string; the bound guards against garbage
    while n < 32768 && unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

fn win_error(what: &str) -> String {
    let code = unsafe { GetLastError() };
    match code {
        ERROR_CANCELLED => "Printing was cancelled.".into(),
        ERROR_ACCESS_DENIED => format!("{what}: access to the printer was denied."),
        0 => format!("{what}."),
        _ => format!("{what} (Windows error {code})."),
    }
}

/// An 8-byte aligned copy of a DEVMODEW for passing to the API.
struct DevBuf(Vec<u64>);

impl DevBuf {
    fn new(bytes: &[u8]) -> Self {
        let mut v = vec![0u64; bytes.len().div_ceil(8).max(size_of::<DEVMODEW>().div_ceil(8))];
        // SAFETY: the u64 buffer holds at least bytes.len() bytes
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), v.as_mut_ptr() as *mut u8, bytes.len()) };
        DevBuf(v)
    }
    fn zeroed(len: usize) -> Self {
        DevBuf(vec![0u64; len.div_ceil(8).max(size_of::<DEVMODEW>().div_ceil(8))])
    }
    fn ptr(&self) -> *const DEVMODEW {
        self.0.as_ptr() as *const DEVMODEW
    }
    fn mut_ptr(&mut self) -> *mut DEVMODEW {
        self.0.as_mut_ptr() as *mut DEVMODEW
    }
    fn bytes(&self, len: usize) -> Vec<u8> {
        // SAFETY: len is clamped to the buffer size
        let len = len.min(self.0.len() * 8);
        unsafe { std::slice::from_raw_parts(self.0.as_ptr() as *const u8, len) }.to_vec()
    }
}

// ---------------------------------------------------------------------------
// Printer handles and DEVMODE
// ---------------------------------------------------------------------------

struct Printer {
    h: PRINTER_HANDLE,
    name: Vec<u16>,
}

impl Printer {
    fn open(name: &str) -> Result<Self, String> {
        if name.trim().is_empty() {
            return Err("No printer is selected.".into());
        }
        let wname = wide(name);
        let mut h = PRINTER_HANDLE { Value: null_mut() };
        if unsafe { OpenPrinterW(wname.as_ptr(), &mut h, null()) } == 0 || h.Value.is_null() {
            let code = unsafe { GetLastError() };
            return Err(match code {
                ERROR_INVALID_PRINTER_NAME | ERROR_PRINTER_NOT_FOUND => format!("The printer \"{name}\" was not found."),
                ERROR_ACCESS_DENIED => format!("Access to the printer \"{name}\" was denied."),
                _ => format!("The printer \"{name}\" could not be opened (Windows error {code})."),
            });
        }
        Ok(Printer { h, name: wname })
    }

    /// Size of the driver's DEVMODEW in bytes.
    fn devmode_size(&self) -> Option<usize> {
        let n = unsafe { DocumentPropertiesW(null_mut(), self.h, self.name.as_ptr(), null_mut(), null(), 0) };
        (n >= size_of::<u16>() as i32 * 34).then_some(n as usize)
    }

    /// The driver's default settings.
    fn default_devmode(&self) -> Option<Vec<u8>> {
        let size = self.devmode_size()?;
        let mut out = DevBuf::zeroed(size);
        let r = unsafe { DocumentPropertiesW(null_mut(), self.h, self.name.as_ptr(), out.mut_ptr(), null(), DM_OUT_BUFFER) };
        let b = out.bytes(size);
        (r == IDOK && devmode_ok(&b)).then_some(b)
    }

    /// Lets the driver validate and merge changed settings.
    fn merge(&self, dm: &[u8]) -> Option<Vec<u8>> {
        let size = self.devmode_size()?;
        let input = DevBuf::new(dm);
        let mut out = DevBuf::zeroed(size);
        let r = unsafe {
            DocumentPropertiesW(null_mut(), self.h, self.name.as_ptr(), out.mut_ptr(), input.ptr(), DM_IN_BUFFER | DM_OUT_BUFFER)
        };
        let b = out.bytes(size);
        (r == IDOK && devmode_ok(&b)).then_some(b)
    }

    fn capability(&self, cap: u16, dm: &[u8]) -> i32 {
        let buf = DevBuf::new(dm);
        unsafe { DeviceCapabilitiesW(self.name.as_ptr(), null(), cap, null_mut(), buf.ptr()) }
    }
}

impl Drop for Printer {
    fn drop(&mut self) {
        unsafe { ClosePrinter(self.h) };
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Calls EnumPrintersW twice (size, then data). Returns an aligned buffer and the entry count.
fn enum_printers(flags: u32, level: u32) -> Option<(Vec<u64>, usize)> {
    let mut needed = 0u32;
    let mut count = 0u32;
    unsafe { EnumPrintersW(flags, null(), level, null_mut(), 0, &mut needed, &mut count) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let ok = unsafe { EnumPrintersW(flags, null(), level, buf.as_mut_ptr() as *mut u8, needed, &mut needed, &mut count) };
    (ok != 0).then_some((buf, count as usize))
}

/// PRINTER_INFO_2W laid out by hand (the windows-sys type needs the Win32_Security feature).
#[repr(C)]
struct PrinterInfo2 {
    _server: *mut u16,
    name: *mut u16,
    _strings: [*mut c_void; 11],
    attributes: u32,
    _priority: [u32; 4],
    status: u32,
    _jobs: u32,
    _ppm: u32,
}

fn default_printer() -> Option<String> {
    let mut n = 0u32;
    unsafe { GetDefaultPrinterW(null_mut(), &mut n) };
    if n == 0 || n > 32768 {
        return None;
    }
    let mut buf = vec![0u16; n as usize];
    (unsafe { GetDefaultPrinterW(buf.as_mut_ptr(), &mut n) } != 0).then(|| unsafe { from_wide(buf.as_ptr()) })
}

/// Installed printers (local and network connections).
pub fn printers() -> Vec<PrinterInfo> {
    let default = default_printer();
    let mut out: Vec<PrinterInfo> = Vec::new();
    if let Some((buf, count)) = enum_printers(PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS, 4) {
        let count = count.min(buf.len() * 8 / size_of::<PRINTER_INFO_4W>());
        let infos = buf.as_ptr() as *const PRINTER_INFO_4W;
        for i in 0..count {
            // SAFETY: the spooler filled `count` entries at the start of the buffer
            let info = unsafe { &*infos.add(i) };
            let name = unsafe { from_wide(info.pPrinterName) };
            if name.is_empty() || out.iter().any(|p| p.name == name) {
                continue;
            }
            out.push(PrinterInfo {
                is_default: default.as_deref() == Some(name.as_str()),
                status: status_text(0, info.Attributes).into(),
                name,
            });
        }
    }
    // Local queues answer level 2 quickly; network connections are left as they are.
    if let Some((buf, count)) = enum_printers(PRINTER_ENUM_LOCAL, 2) {
        let count = count.min(buf.len() * 8 / size_of::<PrinterInfo2>());
        let infos = buf.as_ptr() as *const PrinterInfo2;
        for i in 0..count {
            // SAFETY: as above
            let info = unsafe { &*infos.add(i) };
            let name = unsafe { from_wide(info.name) };
            if let Some(p) = out.iter_mut().find(|p| p.name == name) {
                p.status = status_text(info.status, info.attributes).into();
            }
        }
    }
    sort_printers(&mut out);
    out
}

/// Opens the driver's own properties dialog; updates `devmode` when the user clicks OK.
pub fn printer_properties(hwnd: isize, printer: &str, devmode: &mut Option<Vec<u8>>) -> bool {
    let Ok(p) = Printer::open(printer) else { return false };
    let Some(size) = p.devmode_size() else { return false };
    let input = devmode.as_deref().filter(|b| devmode_matches(b, printer)).map(DevBuf::new);
    let mut out = DevBuf::zeroed(size);
    let mut mode = DM_IN_PROMPT | DM_OUT_BUFFER;
    if input.is_some() {
        mode |= DM_IN_BUFFER;
    }
    let r = unsafe {
        DocumentPropertiesW(hwnd as *mut c_void, p.h, p.name.as_ptr(), out.mut_ptr(), input.as_ref().map_or(null(), |b| b.ptr()), mode)
    };
    let b = out.bytes(size);
    if r == IDOK && devmode_ok(&b) {
        *devmode = Some(b);
        true
    } else {
        false
    }
}

/// Sends the pages to the printer.
pub fn print(pages: &[Page], settings: &PrintSettings) -> Result<(), String> {
    if pages.is_empty() {
        return Err("There's nothing to print.".into());
    }
    let printer = Printer::open(&settings.printer)?;
    let mut base = match settings.devmode.as_ref().filter(|b| devmode_matches(b, &settings.printer)) {
        Some(b) => b.clone(),
        None => printer
            .default_devmode()
            .ok_or_else(|| format!("The settings of the printer \"{}\" could not be read.", settings.printer))?,
    };

    // Copies: let the driver make them when it can, otherwise send the document several times.
    let copies = settings.copies.max(1);
    let max_copies = printer.capability(DC_COPIES, &base);
    let can_collate = printer.capability(DC_COLLATE, &base) == 1;
    let driver_copies = copies == 1 || (max_copies >= copies as i32 && (!settings.collate || can_collate));
    let patch_for = |p: &Page| DevPatch {
        landscape: p.landscape,
        paper: p.paper.code(),
        copies: if driver_copies { copies } else { 1 },
        collate: settings.collate,
        duplex: settings.duplex,
    };
    let configure = |base: &mut Vec<u8>, p: &Page| -> Vec<u8> {
        patch_devmode(base, &patch_for(p));
        printer.merge(base).unwrap_or_else(|| base.clone())
    };

    // Page order, including manual copies.
    let order: Vec<usize> = if driver_copies {
        (0..pages.len()).collect()
    } else if settings.collate {
        (0..copies as usize).flat_map(|_| 0..pages.len()).collect()
    } else {
        (0..pages.len()).flat_map(|i| std::iter::repeat_n(i, copies as usize)).collect()
    };

    let mut current = patch_for(&pages[order[0]]);
    let dm = DevBuf::new(&configure(&mut base, &pages[order[0]]));
    let hdc = unsafe { CreateDCW(wide("WINSPOOL").as_ptr(), printer.name.as_ptr(), null(), dm.ptr()) };
    if hdc.is_null() {
        return Err(win_error(&format!("The printer \"{}\" could not be prepared", settings.printer)));
    }
    let mut job = Job::new(hdc);

    let doc_name = wide(if settings.doc_name.trim().is_empty() { "FreedomOffice Sheets" } else { settings.doc_name.as_str() });
    let output = settings.output_file.as_ref().map(|p| wide(&p.to_string_lossy()));
    let di = DOCINFOW {
        cbSize: size_of::<DOCINFOW>() as i32,
        lpszDocName: doc_name.as_ptr(),
        lpszOutput: output.as_ref().map_or(null(), |o| o.as_ptr()),
        lpszDatatype: null(),
        fwType: 0,
    };
    if unsafe { StartDocW(hdc, &di) } <= 0 {
        return Err(win_error("The printer could not start the document"));
    }
    job.doc_started = true;

    for &i in &order {
        let page = &pages[i];
        let want = patch_for(page);
        if want != current {
            let dm = DevBuf::new(&configure(&mut base, page));
            if unsafe { ResetDCW(hdc, dm.ptr()) }.is_null() {
                return Err(win_error("The printer could not change the paper size or orientation"));
            }
            current = want;
        }
        if unsafe { StartPage(hdc) } <= 0 {
            return Err(win_error("The printer could not start a page"));
        }
        job.draw_page(page);
        if unsafe { EndPage(hdc) } <= 0 {
            return Err(win_error("The printer could not finish a page"));
        }
    }
    if unsafe { EndDoc(hdc) } <= 0 {
        return Err(win_error("The printer could not finish the document"));
    }
    job.doc_started = false;
    Ok(())
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Hash)]
struct FontKey {
    family: String,
    height: i32,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    /// Tenths of a degree, counter-clockwise.
    angle: i32,
}

/// A printer DC with its GDI objects; everything is released on drop.
struct Job {
    hdc: HDC,
    doc_started: bool,
    map: Mapper,
    fonts: HashMap<FontKey, HFONT>,
    pens: HashMap<(u32, u32, bool, bool), HPEN>,
    brushes: HashMap<u32, HBRUSH>,
}

impl Job {
    fn new(hdc: HDC) -> Self {
        Job {
            hdc,
            doc_started: false,
            map: Mapper { dpi_x: 600.0, dpi_y: 600.0, off_x: 0.0, off_y: 0.0 },
            fonts: HashMap::new(),
            pens: HashMap::new(),
            brushes: HashMap::new(),
        }
    }

    fn setup_page(&mut self) {
        let hdc = self.hdc;
        let cap = |i: i32| unsafe { GetDeviceCaps(hdc, i) };
        let dpi = |v: i32| if v > 0 { v as f32 } else { 600.0 };
        self.map = Mapper {
            dpi_x: dpi(cap(LOGPIXELSX)),
            dpi_y: dpi(cap(LOGPIXELSY)),
            off_x: cap(PHYSICALOFFSETX).max(0) as f32,
            off_y: cap(PHYSICALOFFSETY).max(0) as f32,
        };
        unsafe {
            SetGraphicsMode(hdc, GM_ADVANCED);
            SetBkMode(hdc, TRANSPARENT);
        }
    }

    fn brush(&mut self, color: u32) -> HBRUSH {
        *self.brushes.entry(color).or_insert_with(|| unsafe { CreateSolidBrush(colorref(color)) })
    }

    fn pen(&mut self, width_pt: f32, color: u32, dashed: bool, round: bool) -> HPEN {
        let w = self.map.pen(width_pt);
        let dash = self.map.dx(2.0).round().max(1.0) as u32;
        *self.pens.entry((w, color, dashed, round)).or_insert_with(|| {
            let lb = LOGBRUSH { lbStyle: BS_SOLID, lbColor: colorref(color), lbHatch: 0 };
            let join = if round { PS_JOIN_ROUND } else { PS_JOIN_MITER };
            let style = PS_GEOMETRIC | PS_ENDCAP_FLAT | join | if dashed { PS_USERSTYLE } else { PS_SOLID };
            let pattern = [dash, dash];
            let (n, p) = if dashed { (2, pattern.as_ptr()) } else { (0, null()) };
            unsafe { ExtCreatePen(style, w, &lb, n, p) }
        })
    }

    fn font(&mut self, key: FontKey) -> HFONT {
        if let Some(f) = self.fonts.get(&key) {
            return *f;
        }
        let face: Vec<u16> = key.family.encode_utf16().take(31).chain(std::iter::once(0)).collect();
        let f = unsafe {
            CreateFontW(
                -key.height,
                0,
                key.angle,
                key.angle,
                if key.bold { FW_BOLD } else { FW_NORMAL },
                key.italic as u32,
                key.underline as u32,
                key.strike as u32,
                DEFAULT_CHARSET,
                OUT_TT_PRECIS,
                CLIP_DEFAULT_PRECIS | if key.angle != 0 { CLIP_LH_ANGLES } else { 0 },
                ANTIALIASED_QUALITY,
                0,
                face.as_ptr(),
            )
        };
        self.fonts.insert(key, f);
        f
    }

    fn select(&self, obj: *mut c_void) {
        if !obj.is_null() {
            unsafe { SelectObject(self.hdc, obj as HGDIOBJ) };
        }
    }

    fn draw_page(&mut self, page: &Page) {
        self.setup_page();
        for op in &page.ops {
            match op {
                Op::Fill { rect, color } => {
                    let r = self.map.rect(*rect);
                    if r.right > r.left && r.bottom > r.top {
                        let b = self.brush(*color);
                        unsafe { FillRect(self.hdc, &r, b) };
                    }
                }
                Op::Line { x1, y1, x2, y2, width, color, dashed } => {
                    let pen = self.pen(*width, *color, *dashed, false);
                    self.select(pen);
                    let m = self.map;
                    unsafe {
                        MoveToEx(self.hdc, m.x(*x1), m.y(*y1), null_mut());
                        LineTo(self.hdc, m.x(*x2), m.y(*y2));
                    }
                }
                Op::Poly { pts, color } => {
                    if pts.len() < 3 {
                        continue;
                    }
                    let b = self.brush(*color);
                    self.select(b);
                    self.select(unsafe { GetStockObject(NULL_PEN) });
                    let p = self.points(pts);
                    unsafe { Polygon(self.hdc, p.as_ptr(), p.len() as i32) };
                }
                Op::Path { pts, width, color } => {
                    if pts.len() < 2 {
                        continue;
                    }
                    let pen = self.pen(*width, *color, false, true);
                    self.select(pen);
                    let p = self.points(pts);
                    unsafe { Polyline(self.hdc, p.as_ptr(), p.len() as i32) };
                }
                Op::Text(t) => self.text(t),
            }
        }
    }

    fn points(&self, pts: &[(f32, f32)]) -> Vec<POINT> {
        pts.iter().map(|(x, y)| POINT { x: self.map.x(*x), y: self.map.y(*y) }).collect()
    }

    fn extent(&self, s: &[u16]) -> (f32, f32) {
        let mut sz = SIZE { cx: 0, cy: 0 };
        unsafe { GetTextExtentPoint32W(self.hdc, s.as_ptr(), s.len() as i32, &mut sz) };
        (sz.cx as f32, sz.cy as f32)
    }

    fn metrics(&self) -> (f32, f32, f32) {
        let mut tm: TEXTMETRICW = unsafe { std::mem::zeroed() };
        unsafe { GetTextMetricsW(self.hdc, &mut tm) };
        (tm.tmHeight as f32, tm.tmAscent as f32, tm.tmDescent as f32)
    }

    fn out(&self, x: f32, y: f32, s: &[u16]) {
        unsafe { ExtTextOutW(self.hdc, x.round() as i32, y.round() as i32, 0, null(), s.as_ptr(), s.len() as u32, null()) };
    }

    fn text(&mut self, t: &TextOp) {
        let m = self.map;
        let height = m.dy(t.font.size).round() as i32;
        if height < 1 || t.text.is_empty() {
            return;
        }
        let deg = if t.wrap { None } else { rotation_degrees(t.rotation) };
        let key = FontKey {
            family: if t.font.family.trim().is_empty() { "Calibri".into() } else { t.font.family.clone() },
            height,
            bold: t.font.bold,
            italic: t.font.italic,
            underline: t.font.underline,
            strike: t.font.strike,
            angle: deg.map_or(0, |d| (d * 10.0).round() as i32),
        };
        let font = self.font(key);
        let hdc = self.hdc;
        let saved = unsafe { SaveDC(hdc) };
        let clip = m.rect(t.clip);
        unsafe {
            IntersectClipRect(hdc, clip.left, clip.top, clip.right, clip.bottom);
            SetTextColor(hdc, colorref(t.color));
            SetBkMode(hdc, TRANSPARENT);
        }
        self.select(font);

        let pad = m.dx(PAD);
        let (left, right) = (m.fx(t.rect.x), m.fx(t.rect.right()));
        let (top, bottom) = (m.fy(t.rect.y), m.fy(t.rect.bottom()));
        let one = m.dy(1.0);
        let (th, ascent, descent) = self.metrics();
        let valign = |block: f32| match t.valign {
            VAlign::Top => top + one,
            VAlign::Center => top + (bottom - top - block) / 2.0,
            VAlign::Bottom => bottom - block - one,
        };

        if t.wrap {
            let mut r = RECT {
                left: (left + pad + m.dx(t.indent)).round() as i32,
                top: 0,
                right: (right - pad).round() as i32,
                bottom: 0,
            };
            if r.right <= r.left {
                r.right = r.left + 1;
            }
            let s: Vec<u16> = t.text.replace("\r\n", "\n").encode_utf16().collect();
            let fmt = DT_WORDBREAK
                | DT_NOPREFIX
                | match t.halign {
                    Align::Left => DT_LEFT,
                    Align::Center => DT_CENTER,
                    Align::Right => DT_RIGHT,
                };
            let mut calc = r;
            let block = unsafe { DrawTextW(hdc, s.as_ptr(), s.len() as i32, &mut calc, fmt | DT_CALCRECT) } as f32;
            let y = valign(block).round() as i32;
            r.top = y;
            r.bottom = y + block as i32;
            unsafe { DrawTextW(hdc, s.as_ptr(), s.len() as i32, &mut r, fmt | DT_NOCLIP) };
        } else if t.rotation == 255 {
            let chars: Vec<Vec<u16>> = t.text.chars().filter(|c| *c != '\n' && *c != '\r').map(|c| c.to_string().encode_utf16().collect()).collect();
            let y0 = valign(th * chars.len() as f32);
            let cx = (left + right) / 2.0;
            unsafe { SetTextAlign(hdc, TA_LEFT_TOP) };
            for (i, ch) in chars.iter().enumerate() {
                let (w, _) = self.extent(ch);
                self.out(cx - w / 2.0, y0 + th * i as f32, ch);
            }
        } else if let Some(deg) = deg {
            let s: Vec<u16> = t.text.lines().next().unwrap_or("").encode_utf16().collect();
            let (w, _) = self.extent(&s);
            let (minx, maxx, miny, maxy) = rotated_bounds(w, ascent, descent, deg);
            let px = match t.halign {
                Align::Left => left + pad - minx,
                Align::Right => right - pad - maxx,
                Align::Center => (left + right) / 2.0 - (minx + maxx) / 2.0,
            };
            let py = match t.valign {
                VAlign::Top => top + one - miny,
                VAlign::Center => (top + bottom) / 2.0 - (miny + maxy) / 2.0,
                VAlign::Bottom => bottom - one - maxy,
            };
            unsafe { SetTextAlign(hdc, TA_LEFT_BASELINE) };
            self.out(px, py, &s);
        } else {
            let first = t.text.lines().next().unwrap_or("");
            let mut s: Vec<u16> = first.encode_utf16().collect();
            let mut w = self.extent(&s).0;
            let avail = m.dx(t.rect.w - 2.0 * PAD);
            if t.number && w > avail {
                let hw = self.extent(&[b'#' as u16]).0.max(1.0);
                let n = (avail / hw).floor().clamp(1.0, 1000.0) as usize;
                s = vec![b'#' as u16; n];
                w = self.extent(&s).0;
            }
            let x = match t.halign {
                Align::Left => left + pad + m.dx(t.indent),
                Align::Center => (left + right) / 2.0 - w / 2.0,
                Align::Right => right - pad - w,
            };
            unsafe { SetTextAlign(hdc, TA_LEFT_TOP) };
            self.out(x, valign(th), &s);
        }
        unsafe { RestoreDC(hdc, saved) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            if self.doc_started {
                AbortDoc(self.hdc);
            }
            // put stock objects back before deleting ours
            SelectObject(self.hdc, GetStockObject(BLACK_PEN));
            SelectObject(self.hdc, GetStockObject(WHITE_BRUSH));
            SelectObject(self.hdc, GetStockObject(SYSTEM_FONT));
            for f in self.fonts.values() {
                DeleteObject(*f as HGDIOBJ);
            }
            for p in self.pens.values() {
                DeleteObject(*p as HGDIOBJ);
            }
            for b in self.brushes.values() {
                DeleteObject(*b as HGDIOBJ);
            }
            DeleteDC(self.hdc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_swaps_red_and_blue() {
        assert_eq!(colorref(0x112233), 0x332211);
        assert_eq!(colorref(0xFF0000), 0x0000FF);
        assert_eq!(colorref(0x00FF00), 0x00FF00);
        assert_eq!(colorref(0xFFFFFFFF), 0x00FFFFFF);
    }

    #[test]
    fn maps_points_to_device_units() {
        let m = Mapper { dpi_x: 600.0, dpi_y: 300.0, off_x: 50.0, off_y: 25.0 };
        assert_eq!(m.x(72.0), 550);
        assert_eq!(m.y(72.0), 275);
        assert_eq!(m.x(0.0), -50);
        let r = m.rect(PRect::new(36.0, 36.0, 72.0, 72.0));
        assert_eq!((r.left, r.top, r.right, r.bottom), (250, 125, 850, 425));
        assert_eq!(m.pen(0.01), 1);
        assert_eq!(m.pen(0.5), 4);
    }

    fn synthetic_devmode(name: &str, extra: u16) -> Vec<u8> {
        let size = size_of::<DEVMODEW>();
        let mut b = vec![0u8; size + extra as usize];
        for (i, u) in name.encode_utf16().take(31).enumerate() {
            b[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        b[dm::SIZE..dm::SIZE + 2].copy_from_slice(&(size as u16).to_le_bytes());
        b[dm::DRIVER_EXTRA..dm::DRIVER_EXTRA + 2].copy_from_slice(&extra.to_le_bytes());
        b
    }

    #[test]
    fn patches_devmode_fields() {
        let mut b = synthetic_devmode("Office Printer", 16);
        let p = DevPatch { landscape: true, paper: 9, copies: 3, collate: true, duplex: Duplex::LongEdge };
        assert!(patch_devmode(&mut b, &p));
        // SAFETY: the buffer is a full DEVMODEW; read unaligned
        let d: DEVMODEW = unsafe { std::ptr::read_unaligned(b.as_ptr() as *const DEVMODEW) };
        let u = unsafe { d.Anonymous1.Anonymous1 };
        assert_eq!(u.dmOrientation, DMORIENT_LANDSCAPE);
        assert_eq!(u.dmPaperSize, 9);
        assert_eq!(u.dmCopies, 3);
        assert_eq!(d.dmCollate, 1);
        assert_eq!(d.dmDuplex, DMDUP_VERTICAL);
        let need = DM_ORIENTATION | DM_PAPERSIZE | DM_COPIES | DM_COLLATE | DM_DUPLEX;
        assert_eq!(d.dmFields & need, need);
        assert!(devmode_matches(&b, "Office Printer"));
        assert!(!devmode_matches(&b, "Other Printer"));
        // the driver-private tail is untouched and a truncated buffer is rejected
        assert!(b[size_of::<DEVMODEW>()..].iter().all(|x| *x == 0));
        let mut short = b[..b.len() - 1].to_vec();
        assert!(!patch_devmode(&mut short, &p));
        assert!(!devmode_ok(&[0u8; 10]));
    }

    #[test]
    fn long_printer_names_match_truncated_device_name() {
        let long = "\\\\print-server.example.com\\Second Floor Colour Laser";
        let b = synthetic_devmode(long, 0);
        assert_eq!(devmode_device(&b).chars().count(), 31);
        assert!(devmode_matches(&b, long));
    }

    #[test]
    fn status_and_ordering() {
        assert_eq!(status_text(0, 0), "Ready");
        assert_eq!(status_text(0x80, 0), "Offline");
        assert_eq!(status_text(0, PRINTER_ATTRIBUTE_WORK_OFFLINE), "Offline");
        assert_eq!(status_text(0x1, 0), "Paused");
        let p = |n: &str, d: bool| PrinterInfo { name: n.into(), is_default: d, status: "Ready".into() };
        let mut v = vec![p("zeta", false), p("Alpha", false), p("Main", true), p("beta", false)];
        sort_printers(&mut v);
        let names: Vec<&str> = v.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Main", "Alpha", "beta", "zeta"]);
    }

    #[test]
    fn rotated_text_bounds() {
        // unrotated: spans the width, ascent above the baseline, descent below
        assert_eq!(rotated_bounds(100.0, 10.0, 3.0, 0.0), (0.0, 100.0, -10.0, 3.0));
        // 90 degrees counter-clockwise: runs upwards, ascent to the left
        let (minx, maxx, miny, maxy) = rotated_bounds(100.0, 10.0, 3.0, 90.0);
        assert!((minx + 10.0).abs() < 1e-3 && (maxx - 3.0).abs() < 1e-3);
        assert!((miny + 100.0).abs() < 1e-3 && maxy.abs() < 1e-3);
        assert_eq!(rotation_degrees(45), Some(45.0));
        assert_eq!(rotation_degrees(135), Some(-45.0));
        assert_eq!(rotation_degrees(255), None);
        assert_eq!(rotation_degrees(0), None);
    }

    /// Prints a two-page sample to "Microsoft Print to PDF".
    /// Run with `cargo test --release print_to_pdf -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_to_pdf() {
        use crate::model::{cell_from_input, BorderLine, HAlign, Sheet, Style, Workbook};
        use crate::print::{layout, LayoutOptions, PrintWhat};

        let mut wb = Workbook::new();
        wb.sheets.push(Sheet::new("Wide"));
        let header = wb.intern_style(Style { bold: true, fill: Some(0x4472C4), color: Some(0xFFFFFF), halign: HAlign::Center, ..Default::default() });
        let up = wb.intern_style(Style { rotation: 90, bold: true, halign: HAlign::Center, ..Default::default() });
        let down = wb.intern_style(Style { rotation: 135, ..Default::default() });
        let stacked = wb.intern_style(Style { rotation: 255, valign: VAlign::Center, ..Default::default() });
        let wrap = wb.intern_style(Style { wrap: true, valign: VAlign::Top, ..Default::default() });
        let mut b = Style::default();
        b.border.top = BorderLine::Thin;
        b.border.bottom = BorderLine::Thick;
        b.border.left = BorderLine::Dashed;
        b.border.right = BorderLine::Medium;
        b.fill = Some(0xFFF2CC);
        let boxed = wb.intern_style(b);
        let fancy = wb.intern_style(Style { italic: true, underline: true, font: Some("Times New Roman".into()), size10: 140, color: Some(0xC00000), ..Default::default() });
        let strike = wb.intern_style(Style { strike: true, halign: HAlign::Right, valign: VAlign::Center, ..Default::default() });
        let indent = wb.intern_style(Style { indent: 2, halign: HAlign::Left, ..Default::default() });

        let put = |wb: &mut Workbook, si: usize, r: u32, c: u32, text: &str, style: u32| {
            let cell = cell_from_input(wb, text, style);
            wb.sheets[si].set(r, c, cell);
        };
        for (c, h) in ["Item", "Qty", "Price", "Total"].iter().enumerate() {
            put(&mut wb, 0, 0, c as u32, h, header);
        }
        put(&mut wb, 0, 0, 4, "Rotated", up);
        put(&mut wb, 0, 0, 6, "Down", down);
        put(&mut wb, 0, 0, 5, "VERT", stacked);
        wb.sheets[0].row_h.insert(0, 80.0);
        for r in 1..40u32 {
            put(&mut wb, 0, r, 0, &format!("Product number {r}"), 0);
            put(&mut wb, 0, r, 1, &format!("{}", r * 3), 0);
            put(&mut wb, 0, r, 2, &format!("{}", r as f64 * 1.25), 0);
            put(&mut wb, 0, r, 3, &format!("{}", r as f64 * 3.75), 0);
        }
        put(&mut wb, 0, 2, 4, "This long text wraps inside its cell over several lines", wrap);
        wb.sheets[0].row_h.insert(2, 60.0);
        put(&mut wb, 0, 4, 4, "Boxed", boxed);
        put(&mut wb, 0, 6, 4, "Fancy", fancy);
        put(&mut wb, 0, 8, 4, "Struck", strike);
        put(&mut wb, 0, 10, 4, "Indented", indent);
        put(&mut wb, 0, 12, 5, "123456789012", 0);
        wb.sheets[0].col_w.insert(5, 30.0);
        put(&mut wb, 0, 14, 0, "Overflowing text runs into the empty neighbours", 0);
        wb.sheets[0].page.gridlines = true;
        wb.sheets[0].page.headings = true;
        wb.sheets[0].page.row_breaks.insert(20);
        wb.sheets[0].page.footer = crate::page::HeaderFooter::center("Page &P of &N");
        // second sheet: landscape, to exercise ResetDC between pages
        put(&mut wb, 1, 0, 0, "Landscape page", header);
        put(&mut wb, 1, 1, 0, "Second sheet", 0);
        wb.sheets[1].page.landscape = true;

        let o = LayoutOptions { what: PrintWhat::Workbook, selection: None, ignore_print_area: false, file_name: "Sample", file_path: "" };
        let mut pages = layout(&wb, &o, &|_, _, _| Vec::new());
        assert_eq!(pages.len(), 3);
        // chart-style shapes below the table
        pages[0].ops.push(Op::Poly { pts: vec![(100.0, 600.0), (200.0, 520.0), (300.0, 600.0)], color: 0xED7D31 });
        pages[0].ops.push(Op::Path { pts: vec![(100.0, 700.0), (160.0, 640.0), (220.0, 690.0), (300.0, 620.0)], width: 2.0, color: 0x4472C4 });

        let out = std::env::temp_dir().join("freedom_sheets_print_test.pdf");
        let _ = std::fs::remove_file(&out);
        let settings = PrintSettings {
            printer: "Microsoft Print to PDF".into(),
            copies: 1,
            collate: true,
            duplex: Duplex::OneSided,
            devmode: None,
            doc_name: "FreedomOffice print test".into(),
            output_file: Some(out.clone()),
        };
        println!("printers: {:?}", printers());
        print(&pages, &settings).expect("print");
        // the spooler writes the file asynchronously
        let mut len = 0;
        for _ in 0..100 {
            len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            if len > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        println!("wrote {} ({len} bytes)", out.display());
        assert!(len > 0, "no PDF written");
    }
}
