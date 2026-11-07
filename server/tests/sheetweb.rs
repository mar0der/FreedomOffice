//! Web Sheets end to end: the browser viewer's data endpoints (`/api/items/{id}/sheet...`) and
//! page (`/drive/sheet/{id}`) against a real server on 127.0.0.1 with throwaway accounts. Files
//! are written with `rust_xlsxwriter` (so the app's own reader and engine produce the answers),
//! plus hostile ones.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use cosync::{CellData, Op, Val};
use freedomoffice_server::config::Config;
use freedomoffice_server::store::{LiveOp, Store};
use freedomoffice_server::{spawn, Running};
use rust_xlsxwriter::{Format, Formula, Workbook};
use serde_json::{json, Value};

/// Test-only password for the throwaway accounts below.
const PW: &str = "test-password-1";
const ORIGIN: &str = "https://drive.example.test";
const COOKIE: &str = "__Host-fo_web";

struct Srv {
    run: Option<Running>,
    url: String,
    dir: PathBuf,
    cfg: Config,
    agent: ureq::Agent,
}

impl Drop for Srv {
    fn drop(&mut self) {
        drop(self.run.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

fn start() -> Srv {
    let dir = std::env::temp_dir().join(format!("fo-sheetweb-test-{}", freedomoffice_server::util::new_id()));
    let cfg = Config { data_dir: dir.join("data"), login_delay_ms: 10, public_url: ORIGIN.into(), ..Config::default() };
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        s.create_user("alice", "Alice Admin", "", PW, true).unwrap();
        s.create_user("bob", "Bob Builder", "", PW, false).unwrap();
        s.create_user("carol", "Carol C", "", PW, false).unwrap();
    }
    let run = spawn(cfg.clone()).unwrap();
    let url = run.url();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_global(Some(Duration::from_secs(60))).build().into();
    Srv { run: Some(run), url, dir, cfg, agent }
}

impl Srv {
    fn store(&self) -> Store {
        Store::open(&self.cfg.db_path(), &self.cfg.files_dir(), self.cfg.versions_to_keep).unwrap()
    }

    fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Vec<u8>>) -> Resp {
        let mut b = ureq::http::Request::builder().method(method).uri(format!("{}{path}", self.url));
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let res = match body {
            Some(data) => self.agent.run(b.body(data).unwrap()),
            None => self.agent.run(b.body(()).unwrap()),
        };
        let mut res = res.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let body = res.body_mut().read_to_vec().unwrap_or_default();
        Resp { status, body, headers }
    }

    fn token(&self, user: &str) -> String {
        let r = self.send("POST", "/api/login", &[("Content-Type", "application/json")], Some(json!({ "username": user, "password": PW }).to_string().into_bytes()));
        assert_eq!(r.status, 200, "{}", r.text());
        r.json()["token"].as_str().unwrap().to_string()
    }

    fn bearer(&self, path: &str, token: &str) -> Resp {
        self.send("GET", path, &[("Authorization", &format!("Bearer {token}"))], None)
    }

    fn post(&self, path: &str, token: &str, body: Value) -> Resp {
        self.send("POST", path, &[("Authorization", &format!("Bearer {token}")), ("Content-Type", "application/json")], Some(body.to_string().into_bytes()))
    }

    /// A browser session: cookie and nothing else.
    fn web(&self, user: &str) -> String {
        let r = self.send("POST", "/api/web/login", &[("Content-Type", "application/json"), ("Origin", ORIGIN)], Some(json!({ "username": user, "password": PW }).to_string().into_bytes()));
        assert_eq!(r.status, 200, "{}", r.text());
        let set = r.header("set-cookie").unwrap();
        let secret = set.strip_prefix(&format!("{COOKIE}=")).unwrap().split(';').next().unwrap();
        format!("{COOKIE}={secret}")
    }

    fn cookie_get(&self, path: &str, cookie: &str) -> Resp {
        self.send("GET", path, &[("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")], None)
    }

    fn root(&self, token: &str) -> String {
        self.bearer("/api/root", token).json()["id"].as_str().unwrap().to_string()
    }

    fn upload(&self, token: &str, parent: &str, name: &str, data: &[u8]) -> String {
        let r = self.send("POST", &format!("/api/files?parent={parent}&name={name}"), &[("Authorization", &format!("Bearer {token}"))], Some(data.to_vec()));
        assert_eq!(r.status, 201, "{}", r.text());
        r.json()["id"].as_str().unwrap().to_string()
    }
}

fn book() -> Vec<u8> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Budget").unwrap();
    let bold = Format::new().set_bold().set_background_color(0xffee99).set_num_format("0.00");
    let pct = Format::new().set_num_format("0.0%");
    ws.write_string(0, 0, "Item").unwrap();
    ws.write_string(0, 1, "Cost").unwrap();
    ws.write_string(1, 0, "Rent").unwrap();
    ws.write_number_with_format(1, 1, 1200.5, &bold).unwrap();
    ws.write_string(2, 0, "Food").unwrap();
    ws.write_number(2, 1, 300.0).unwrap();
    ws.write_string(3, 0, "Total").unwrap();
    ws.write_formula(3, 1, Formula::new("=SUM(B2:B3)")).unwrap();
    ws.write_number_with_format(4, 1, 0.256, &pct).unwrap();
    ws.merge_range(6, 0, 6, 2, "A merged title", &Format::new().set_bold()).unwrap();
    ws.set_freeze_panes(1, 1).unwrap();
    ws.set_column_width(2, 30.0).unwrap();
    ws.set_column_hidden(3).unwrap();
    ws.set_row_hidden(8).unwrap();
    let ws2 = wb.add_worksheet();
    ws2.set_name("Second").unwrap();
    ws2.write_string(0, 0, "=not a formula").unwrap();
    ws2.write_formula(1, 0, Formula::new("=Budget!B4*2")).unwrap();
    wb.save_to_buffer().unwrap()
}

fn cell(w: &Value, r: u64, c: u64) -> Option<&Value> {
    w["cells"].as_array().unwrap().iter().find(|x| x[0] == r && x[1] == c)
}

fn text_of(w: &Value, r: u64, c: u64) -> String {
    cell(w, r, c).map(|x| x[2].as_str().unwrap().to_string()).unwrap_or_default()
}

fn url(id: &str, rest: &str) -> String {
    format!("/api/items/{id}/sheet{rest}")
}

#[test]
fn a_viewer_sees_values_formats_and_layout() {
    let s = start();
    let a = s.token("alice");
    let root = s.root(&a);
    let id = s.upload(&a, &root, "Budget.xlsx", &book());
    assert_eq!(s.post(&format!("/api/items/{id}/permissions"), &a, json!({ "username": "bob", "role": "view" })).status, 200);
    let bob = s.web("bob");

    let m = s.cookie_get(&url(&id, ""), &bob);
    assert_eq!(m.status, 200, "{}", m.text());
    assert_eq!(m.header("cache-control"), Some("private, no-store"));
    let m = m.json();
    assert_eq!(m["role"], "view");
    assert_eq!(m["item"]["name"], "Budget.xlsx");
    let sheets = m["sheets"].as_array().unwrap();
    assert_eq!(sheets.len(), 2);
    assert_eq!(sheets[0]["name"], "Budget");
    assert_eq!(sheets[0]["freeze"], json!([1, 1]));
    assert_eq!(sheets[0]["hidden_cols"], json!([[3, 3]]));
    assert_eq!(sheets[0]["hidden_rows"], json!([[8, 8]]));
    assert_eq!(sheets[0]["merges"], json!([[6, 0, 6, 2]]));
    assert!(sheets[0]["col_w"].as_array().unwrap().iter().any(|x| x[0] == 2));
    assert!(sheets[0]["rows"].as_u64().unwrap() >= 7);

    let w = s.cookie_get(&url(&id, "/cells?sheet=0&r1=0&r2=20&c1=0&c2=5"), &bob);
    assert_eq!(w.status, 200, "{}", w.text());
    let w = w.json();
    assert_eq!(w["rev"], m["rev"]);
    // Values computed by the app's engine, number formats applied.
    assert_eq!(text_of(&w, 1, 1), "1200.50");
    assert_eq!(text_of(&w, 2, 1), "300");
    assert_eq!(text_of(&w, 3, 1), "1500.5");
    assert_eq!(text_of(&w, 4, 1), "25.6%");
    assert_eq!(text_of(&w, 0, 0), "Item");
    assert_eq!(cell(&w, 3, 1).unwrap()[3], "n");
    assert_eq!(cell(&w, 0, 0).unwrap()[3], "s");
    // Styles: bold, fill.
    let si = cell(&w, 1, 1).unwrap()[4].as_u64().unwrap() as usize;
    let st = &w["styles"][si - 1];
    assert_eq!((st["b"].as_i64(), st["f"].as_str()), (Some(1), Some("#ffee99")));
    // A merged area's text sits in its anchor.
    assert_eq!(text_of(&w, 6, 0), "A merged title");
    assert_eq!(w["merges"], json!([[6, 0, 6, 2]]));

    // The second sheet: text that looks like a formula stays text; a formula over another sheet.
    let w2 = s.cookie_get(&url(&id, "/cells?sheet=1&r1=0&r2=5&c1=0&c2=2"), &bob).json();
    assert_eq!(text_of(&w2, 0, 0), "=not a formula");
    assert_eq!(text_of(&w2, 1, 0), "3001");

    // The formula bar: the formula for viewers too.
    let c = s.cookie_get(&url(&id, "/cell?sheet=0&r=3&c=1"), &bob).json();
    assert_eq!((c["input"].as_str(), c["text"].as_str(), c["formula"].as_bool()), (Some("=SUM(B2:B3)"), Some("1500.5"), Some(true)));
    // Inside a merged area the anchor answers.
    let c = s.cookie_get(&url(&id, "/cell?sheet=0&r=6&c=2"), &bob).json();
    assert_eq!((c["r"].as_u64(), c["c"].as_u64(), c["input"].as_str()), (Some(6), Some(0), Some("A merged title")));

    // Bad windows.
    for q in ["sheet=0&r1=5&r2=2&c1=0&c2=1", "sheet=0&r1=0&r2=5000&c1=0&c2=1", "sheet=0&r1=0&r2=999&c1=0&c2=100", "sheet=9&r1=0&r2=1&c1=0&c2=1", "sheet=0&r1=0&r2=1&c1=0", "sheet=0&r1=0&r2=2000000&c1=0&c2=1"] {
        let r = s.cookie_get(&url(&id, &format!("/cells?{q}")), &bob);
        assert_eq!(r.status, 400, "{q}: {}", r.text());
    }
    // CSV of a sheet, with the safe headers.
    let r = s.cookie_get(&url(&id, "/csv?sheet=0"), &bob);
    assert_eq!(r.status, 200);
    assert!(r.text().contains("Rent,1200.50"), "{}", r.text());
    assert!(r.header("content-disposition").unwrap().starts_with("attachment"));
    let r2 = s.cookie_get(&url(&id, "/csv?sheet=1"), &bob);
    assert!(r2.text().starts_with("'=not a formula"), "formula injection guard: {}", r2.text());
}

#[test]
fn access_is_the_content_endpoints_access() {
    let s = start();
    let a = s.token("alice");
    let root = s.root(&a);
    let id = s.upload(&a, &root, "Private.xlsx", &book());
    let folder = {
        let r = s.post("/api/folders", &a, json!({ "parent": root, "name": "Dir" }));
        r.json()["id"].as_str().unwrap().to_string()
    };
    let carol = s.web("carol");
    for rest in ["", "/cells?sheet=0&r1=0&r2=1&c1=0&c2=1", "/cell?sheet=0&r=0&c=0", "/csv?sheet=0"] {
        // A stranger can't tell the file exists; nobody signed in is asked to sign in.
        assert_eq!(s.cookie_get(&url(&id, rest), &carol).status, 404, "{rest}");
        assert_eq!(s.send("GET", &url(&id, rest), &[], None).status, 401, "{rest}");
        assert_eq!(s.bearer(&url(&id, rest), "not-a-token").status, 401);
    }
    assert_eq!(s.cookie_get(&url("nonexistent", ""), &carol).status, 404);
    // A folder isn't a workbook.
    assert_eq!(s.bearer(&url(&folder, ""), &a).status, 400);
    // The owner, by bearer token.
    assert_eq!(s.bearer(&url(&id, ""), &a).status, 200);
    // Cross-site requests with the cookie are refused like everywhere else.
    let alice = s.web("alice");
    let r = s.send("GET", &url(&id, ""), &[("Cookie", &alice), ("Sec-Fetch-Site", "cross-site")], None);
    assert_eq!(r.status, 403);

    // Sharing gives a viewer access; revoking takes it away at once, though the book is cached.
    let b = s.token("bob");
    assert_eq!(s.bearer(&url(&id, ""), &b).status, 404);
    assert_eq!(s.post(&format!("/api/items/{id}/permissions"), &a, json!({ "username": "bob", "role": "view" })).status, 200);
    assert_eq!(s.bearer(&url(&id, ""), &b).status, 200);
    assert_eq!(s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=1&c1=0&c2=1"), &b).status, 200);
    let r = s.send("DELETE", &format!("/api/items/{id}/permissions/bob"), &[("Authorization", &format!("Bearer {a}"))], None);
    assert!(r.status < 300, "{}", r.text());
    assert_eq!(s.bearer(&url(&id, ""), &b).status, 404);
    assert_eq!(s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=1&c1=0&c2=1"), &b).status, 404);

    // A share link (view) with its access token.
    let l = s.post(&format!("/api/items/{id}/links"), &a, json!({ "role": "view" }));
    assert_eq!(l.status, 201, "{}", l.text());
    let token = l.json()["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let access = s.send("GET", &format!("/api/links/{token}"), &[], None).json()["access_token"].as_str().unwrap().to_string();
    let m = s.bearer(&url(&id, ""), &access);
    assert_eq!(m.status, 200, "{}", m.text());
    assert_eq!(m.json()["link"], true);
    assert_eq!(m.json()["item"]["parent"], Value::Null, "a link holder isn't told where the file lives");
    assert_eq!(s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=3&c1=0&c2=2"), &access).status, 200);
    // Not a bit of any other file.
    let other = s.upload(&a, &root, "Other.xlsx", &book());
    assert_eq!(s.bearer(&url(&other, ""), &access).status, 404);
    // Trashed files are gone for the viewer.
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}"), &[("Authorization", &format!("Bearer {a}"))], None).status, 204);
    assert_eq!(s.bearer(&url(&id, ""), &access).status, 404);
}

#[test]
fn files_that_cant_be_shown_say_so() {
    let s = start();
    let a = s.token("alice");
    let root = s.root(&a);

    // Garbage with an .xlsx name, an empty file, a truncated archive.
    let good = book();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("garbage.xlsx", (0..5000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()),
        ("empty.xlsx", Vec::new()),
        ("cut.xlsx", good[..good.len() / 2].to_vec()),
        ("text.xlsx", b"hello, this is not a workbook".to_vec()),
    ];
    for (name, data) in cases {
        let id = s.upload(&a, &root, name, &data);
        let r = s.bearer(&url(&id, ""), &a);
        assert_eq!(r.status, 422, "{name}: {}", r.text());
        assert_eq!(r.json()["code"], "unreadable", "{name}");
        assert!(!r.json()["error"].as_str().unwrap().is_empty());
        // The same answer from the other endpoints (the failure is remembered).
        assert_eq!(s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=1&c1=0&c2=1"), &a).status, 422);
    }
    // A zip with valid structure but broken workbook parts.
    let mut z = zip_writer();
    z.add("[Content_Types].xml", b"<Types/>");
    z.add("xl/workbook.xml", b"<workbook><sheets><sheet name=\"A\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>");
    z.add("xl/worksheets/sheet1.xml", b"<worksheet><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>99999</v></c><c r=\"ZZZZZ99999999\"/></row></sheetData></worksheet>");
    let id = s.upload(&a, &root, "weird.xlsx", &z.finish());
    let r = s.bearer(&url(&id, ""), &a);
    assert!(r.status == 200 || r.status == 422, "{}", r.text());

    // Formats the browser doesn't read: a clear message and a way to download.
    for name in ["old.xls", "sheet.ods", "book.xlsb", "notes.docx"] {
        let id = s.upload(&a, &root, name, b"whatever");
        let r = s.bearer(&url(&id, ""), &a);
        assert_eq!(r.status, 415, "{name}");
        assert_eq!(r.json()["code"], "unsupported");
        assert!(r.json()["error"].as_str().unwrap().contains("Download"));
        // ...and the download itself works for this person.
        assert_eq!(s.bearer(&format!("/api/items/{id}/content?download=1"), &a).status, 200);
    }
    // A zip bomb: a small archive that unpacks to far more than the limit.
    let mut z = zip_writer();
    z.add("[Content_Types].xml", b"<Types/>");
    z.add_big("xl/worksheets/big.xml", 150 * 1024 * 1024);
    let bomb = z.finish();
    assert!(bomb.len() < 2 * 1024 * 1024, "the bomb is small: {}", bomb.len());
    let id = s.upload(&a, &root, "bomb.xlsx", &bomb);
    let r = s.bearer(&url(&id, ""), &a);
    assert_eq!(r.status, 413, "{}", r.text());
    assert_eq!(r.json()["code"], "too_large");
    // The server still answers.
    assert_eq!(s.send("GET", "/api/health", &[], None).status, 200);
}

/// Builds zip archives (deflated) for the hostile-file tests.
struct ZipBuilder(zip::ZipWriter<std::io::Cursor<Vec<u8>>>);

fn zip_writer() -> ZipBuilder {
    ZipBuilder(zip::ZipWriter::new(std::io::Cursor::new(Vec::new())))
}

impl ZipBuilder {
    fn opts() -> zip::write::SimpleFileOptions {
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true)
    }

    fn add(&mut self, name: &str, data: &[u8]) {
        self.0.start_file(name, Self::opts()).unwrap();
        self.0.write_all(data).unwrap();
    }

    /// A part of `size` zero bytes (a few KB once deflated).
    fn add_big(&mut self, name: &str, size: usize) {
        self.0.start_file(name, Self::opts()).unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..size / chunk.len() {
            self.0.write_all(&chunk).unwrap();
        }
    }

    fn finish(self) -> Vec<u8> {
        self.0.finish().unwrap().into_inner()
    }
}

#[test]
fn big_sheets_are_sent_in_windows() {
    let s = start();
    let a = s.token("alice");
    let root = s.root(&a);
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    for r in 0..30_000u32 {
        ws.write_number(r, 0, f64::from(r)).unwrap();
        ws.write_string(r, 1, format!("row {r}")).unwrap();
        ws.write_number(r, 9, f64::from(r) * 2.0).unwrap();
    }
    // A cell far away: the sheet's extent is real, the rest is not materialised.
    ws.write_string(900_000, 40, "far").unwrap();
    let id = s.upload(&a, &root, "web-big.xlsx", &wb.save_to_buffer().unwrap());
    let t0 = std::time::Instant::now();
    let m = s.bearer(&url(&id, ""), &a).json();
    assert_eq!(m["sheets"][0]["rows"], 900_001);
    assert_eq!(m["sheets"][0]["cols"], 41);
    assert!(m["cells"].as_u64().unwrap() >= 90_000);
    let w = s.bearer(&url(&id, "/cells?sheet=0&r1=20000&r2=20099&c1=0&c2=20"), &a);
    assert_eq!(w.status, 200);
    let wj = w.json();
    assert_eq!(text_of(&wj, 20_050, 1), "row 20050");
    assert_eq!(text_of(&wj, 20_050, 9), "40100");
    assert_eq!(wj["cells"].as_array().unwrap().len(), 300);
    assert!(w.body.len() < 40_000, "a window is small: {}", w.body.len());
    let far = s.bearer(&url(&id, "/cells?sheet=0&r1=899990&r2=900010&c1=38&c2=42"), &a).json();
    assert_eq!(text_of(&far, 900_000, 40), "far");
    // Empty regions cost nothing.
    let none = s.bearer(&url(&id, "/cells?sheet=0&r1=500000&r2=500099&c1=0&c2=20"), &a).json();
    assert_eq!(none["cells"].as_array().unwrap().len(), 0);
    // Windows come from the open book: many are quick.
    let t1 = std::time::Instant::now();
    for i in 0..30 {
        let r = s.bearer(&url(&id, &format!("/cells?sheet=0&r1={}&r2={}&c1=0&c2=20", i * 100, i * 100 + 99)), &a);
        assert_eq!(r.status, 200);
    }
    assert!(t1.elapsed() < Duration::from_secs(10), "30 windows took {:?} (first open {:?})", t1.elapsed(), t0.elapsed());
    // The window limits.
    assert_eq!(s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=1000&c1=0&c2=5"), &a).status, 400);
    assert_eq!(s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=999&c1=0&c2=60"), &a).status, 400);
}

#[test]
fn csv_files_open_as_a_sheet() {
    let s = start();
    let a = s.token("alice");
    let root = s.root(&a);
    let id = s.upload(&a, &root, "people.csv", "name,age,note\nAnn,31,\"likes, commas\"\nBo,4,=1+1\n".as_bytes());
    let m = s.bearer(&url(&id, ""), &a).json();
    assert_eq!(m["sheets"][0]["name"], "people");
    let w = s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=5&c1=0&c2=5"), &a).json();
    assert_eq!(text_of(&w, 1, 2), "likes, commas");
    assert_eq!(text_of(&w, 1, 1), "31");
    assert_eq!(text_of(&w, 2, 2), "=1+1", "formulas in csv stay text");
}

#[test]
fn the_current_state_includes_the_live_log() {
    let s = start();
    let a = s.token("alice");
    let root = s.root(&a);
    let id = s.upload(&a, &root, "web-live.xlsx", &book());
    let m1 = s.bearer(&url(&id, ""), &a).json();
    // A live session has committed changes no snapshot has covered: cell B3 (row 2, col 1)
    // becomes 1000, so the total changes.
    {
        let st = s.store();
        let item = st.item(&id).unwrap().unwrap();
        let base = st.live_base(&item).unwrap();
        let ops = vec![Op::Content { s: 1, r: 2, c: 1, v: Some(Box::new(CellData { v: Val::Num(1000.0), ..Default::default() })), adj: vec![] }];
        st.add_live_op(&id, &LiveOp { seq: base.seq + 1, cid: "client-1".into(), batch: 1, actor: "alice".into(), ops: serde_json::to_string(&ops).unwrap(), at: 1 }).unwrap();
    }
    let m2 = s.bearer(&url(&id, ""), &a).json();
    assert_ne!(m1["rev"], m2["rev"], "the revision follows the live log");
    let w = s.bearer(&url(&id, "/cells?sheet=0&r1=0&r2=5&c1=0&c2=2"), &a).json();
    assert_eq!(text_of(&w, 2, 1), "1000");
    assert_eq!(text_of(&w, 3, 1), "2200.5", "formulas follow the live changes");
    // A window fetched with the old revision tells the viewer to reload (the rev differs).
    assert_eq!(w["rev"], m2["rev"]);
}

#[test]
fn the_pages_are_served_with_a_strict_policy() {
    let s = start();
    let r = s.send("GET", "/drive/sheet/anything-1", &[], None);
    assert_eq!(r.status, 200);
    let csp = r.header("content-security-policy").unwrap();
    assert!(csp.contains("script-src 'self'") && csp.contains("default-src 'none'") && !csp.contains("unsafe"), "{csp}");
    assert_eq!(r.header("x-frame-options"), Some("DENY"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let html = r.text();
    assert!(html.contains("/drive/assets/sheet.js?v="));
    // The scripts and style come from the assets; the page is not an asset.
    let js = s.send("GET", "/drive/assets/sheet.js", &[], None);
    assert_eq!(js.status, 200);
    assert!(js.header("content-type").unwrap().starts_with("text/javascript"));
    assert_eq!(s.send("GET", "/drive/assets/sheet.html", &[], None).status, 404);
    assert_eq!(s.send("GET", "/drive/assets/sheet.css", &[], None).status, 200);
}
