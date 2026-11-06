//! Crash reporting: a panic hook that saves a report (and, on Linux, pops a desktop
//! notification right away), detection of native crashes through systemd-coredump on the
//! next start, and a prefilled GitHub issue the user can review and send themselves.
//! Nothing is uploaded automatically.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ISSUES_URL: &str = "https://github.com/mar0der/FreedomOffice/issues/new";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Keep the issue URL well under the length browsers and GitHub accept.
const MAX_BODY: usize = 6000;

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// Short line for the issue title, e.g. "panic at src/model.rs:10" or "SIGSEGV".
    pub summary: String,
    /// Markdown issue body (what the user sees before sending).
    pub body: String,
    /// The saved panic report this came from (deleted once handled).
    pub file: Option<PathBuf>,
}

impl Report {
    pub fn issue_url(&self) -> String {
        let title = format!("Crash: {} (FreedomSheets {VERSION})", self.summary);
        let body = truncate(&self.body, MAX_BODY);
        format!("{ISSUES_URL}?labels=crash&title={}&body={}", crate::cloud::pct_encode(&title), crate::cloud::pct_encode(&body))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…(truncated)\n```", &s[..end])
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Where crash reports live.
pub fn crash_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("FreedomOffice"));
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Logs/FreedomOffice"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .map(|p| p.join("freedomoffice"));
    base.map(|b| b.join("crashes"))
}

fn environment() -> String {
    let mut s = format!("- Version: FreedomSheets {VERSION}\n- OS: {} {}\n", std::env::consts::OS, std::env::consts::ARCH);
    #[cfg(target_os = "linux")]
    {
        if let Ok(t) = std::fs::read_to_string("/etc/os-release") {
            if let Some(n) = t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")) {
                s.push_str(&format!("- Distribution: {}\n", n.trim_matches('"')));
            }
        }
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        if !session.is_empty() || !desktop.is_empty() {
            s.push_str(&format!("- Session: {session} {desktop}\n"));
        }
    }
    s
}

/// Removes the user's home directory from paths in reports.
fn scrub(text: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if h.len() > 1 => text.replace(&h, "~"),
        _ => text.to_string(),
    }
}

fn panic_body(message: &str, location: &str, thread: &str, backtrace: &str) -> String {
    scrub(&format!(
        "**What happened**\nFreedomSheets stopped with an internal error (panic).\n\n**What I was doing**\n<!-- please describe -->\n\n\
         **Details**\n{}- Thread: {thread}\n- Location: {location}\n- Message: {message}\n\n```\n{backtrace}\n```",
        environment()
    ))
}

/// Installs the panic hook. Call first thing in `main`.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let body = panic_body(&message, &location, &thread, &backtrace);
        if let Some(dir) = crash_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let text = format!("summary: panic at {location}\n\n{body}");
            let _ = std::fs::write(dir.join(format!("panic-{}.md", now())), text);
        }
        notify_now();
        previous(info);
    }));
}

/// Desktop notification at the moment of the crash (the app is about to exit).
fn notify_now() {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "FreedomSheets", "-u", "critical", "FreedomSheets crashed", "Open FreedomSheets again to send a crash report."])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

fn read_panic_report(path: &Path) -> Option<Report> {
    let text = std::fs::read_to_string(path).ok()?;
    let (head, body) = text.split_once("\n\n")?;
    let summary = head.strip_prefix("summary: ").unwrap_or("panic").to_string();
    Some(Report { summary, body: body.to_string(), file: Some(path.to_path_buf()) })
}

/// The newest unhandled crash, if any: a saved panic report, or (Linux) a core dump of this
/// program newer than the last check. Older panic reports are discarded.
pub fn pending_report() -> Option<Report> {
    let dir = crash_dir()?;
    let mut panics: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("panic-") && n.ends_with(".md")))
                .collect()
        })
        .unwrap_or_default();
    panics.sort();
    let newest = panics.pop();
    for old in panics {
        let _ = std::fs::remove_file(old);
    }
    let core = native_crash_since(&dir);
    newest.and_then(|p| read_panic_report(&p)).or(core)
}

/// Marks a report as handled (sent or dismissed).
pub fn dismiss(report: &Report) {
    if let Some(f) = &report.file {
        let _ = std::fs::remove_file(f);
    }
}

/// Linux: asks systemd-coredump for crashes of this executable since the last start.
#[cfg(target_os = "linux")]
fn native_crash_since(dir: &Path) -> Option<Report> {
    let stamp = dir.join("last-core-check");
    let since = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse::<u64>().ok());
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&stamp, now().to_string());
    // First run: only start watching from now on.
    let since = since?;
    let exe = std::env::current_exe().ok()?;
    let out = std::process::Command::new("coredumpctl")
        .args(["--no-pager", "--no-legend", "--json=short", &format!("--since=@{since}"), "list"])
        .arg(&exe)
        .env("LC_ALL", "C")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let last = list.as_array()?.last()?;
    let pid = last.get("pid").and_then(|v| v.as_u64())?;
    let sig = last.get("sig").and_then(|v| v.as_u64()).unwrap_or(0);
    let signal = signal_name(sig);
    let info = std::process::Command::new("coredumpctl")
        .args(["--no-pager", "info", &pid.to_string()])
        .env("LC_ALL", "C")
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let body = scrub(&format!(
        "**What happened**\nFreedomSheets crashed ({signal}).\n\n**What I was doing**\n<!-- please describe -->\n\n\
         **Details**\n{}\n```\n{}\n```",
        environment(),
        stack_excerpt(&info)
    ));
    Some(Report { summary: signal, body, file: None })
}

#[cfg(not(target_os = "linux"))]
fn native_crash_since(_dir: &Path) -> Option<Report> {
    None
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn signal_name(sig: u64) -> String {
    match sig {
        4 => "SIGILL".into(),
        6 => "SIGABRT".into(),
        7 => "SIGBUS".into(),
        8 => "SIGFPE".into(),
        11 => "SIGSEGV".into(),
        n => format!("signal {n}"),
    }
}

/// `coredumpctl info` summary lines plus the first stack trace (the crashing thread).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stack_excerpt(info: &str) -> String {
    let mut out = Vec::new();
    let mut in_stack = false;
    for line in info.lines() {
        let t = line.trim_start();
        if t.starts_with("Signal:") || t.starts_with("Timestamp:") || t.starts_with("Command Line:") {
            out.push(t.to_string());
        } else if t.starts_with("Stack trace of thread") {
            if in_stack {
                break;
            }
            in_stack = true;
            out.push(t.to_string());
        } else if in_stack {
            if t.is_empty() {
                break;
            }
            out.push(t.to_string());
            if out.len() > 60 {
                break;
            }
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_excerpt_takes_first_thread() {
        let info = "           PID: 12 (FreedomSheets)\n        Signal: 11 (SEGV)\n     Timestamp: Thu\n\
                    Stack trace of thread 12:\n#0  0x1 foo (FreedomSheets + 0x1)\n#1  0x2 bar\n\n\
                    Stack trace of thread 13:\n#0  0x3 baz\n";
        let s = stack_excerpt(info);
        assert!(s.contains("Signal: 11"));
        assert!(s.contains("#1  0x2 bar"));
        assert!(!s.contains("baz"));
    }

    #[test]
    fn issue_url_is_encoded_and_bounded() {
        let r = Report { summary: "panic at a.rs:1".into(), body: "x & y\n".repeat(5000), file: None };
        let url = r.issue_url();
        assert!(url.starts_with(ISSUES_URL));
        assert!(!url.contains(' ') && !url.contains('\n'));
        assert!(url.len() < 3 * MAX_BODY + 500);
    }

    #[test]
    fn panic_report_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fo-crash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("panic-1.md");
        std::fs::write(&p, format!("summary: panic at x.rs:2\n\n{}", panic_body("boom", "x.rs:2", "ui", "bt"))).unwrap();
        let r = read_panic_report(&p).unwrap();
        assert_eq!(r.summary, "panic at x.rs:2");
        assert!(r.body.contains("Message: boom"));
        dismiss(&r);
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
