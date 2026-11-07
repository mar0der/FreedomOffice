//! Web Sheets, the server side: the current state of a workbook as JSON for the browser viewer
//! (`/drive/sheet/{id}`, see SPEC.md "Web Sheets").
//!
//! * `GET /api/items/{id}/sheet`: the sheets (names, sizes, widths, merges, frozen panes...), no cells.
//! * `GET /api/items/{id}/sheet/cells?sheet=&r1=&r2=&c1=&c2=`: the cells of a window.
//! * `GET /api/items/{id}/sheet/cell?sheet=&r=&c=`: one cell for the formula bar.
//! * `GET /api/items/{id}/sheet/csv?sheet=`: a sheet as CSV (displayed values).
//! * `GET /api/items/{id}/sheet/image?sheet=&n=`: a picture; `GET .../sheet/charts?sheet=`: charts with their data.
//!
//! Access is the content endpoint's: the same authentication (bearer token, web session, share
//! link) and a viewer role is enough. The workbook is the saved file version plus the live log
//! of a co-editing session that no snapshot has covered yet, so the browser shows what the
//! people editing see. A hostile file can't take the server down: conversions run one thread
//! each (large stack, panics caught), at most `CONVERSIONS` at a time, each for at most
//! `CONVERT_TIMEOUT`; a conversion that overruns is abandoned (its thread and slot are given
//! back when it ends, if it ever does), and its failure is remembered so the file isn't tried
//! again at every request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use sheetview::{Book, LiveInput, LoadError};
use tokio::sync::{OnceCell, Semaphore};

use crate::api::{self, ApiError, ApiResult, App};
use crate::store::{Item, Kind, Principal, Role, Store};

/// Files above this are not opened in the browser.
pub const MAX_VIEW_BYTES: u64 = 60 * 1024 * 1024;
/// Conversions running at once.
const CONVERSIONS: usize = 3;
/// How long one conversion may take.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(25);
/// How long a request waits for a free conversion slot.
const SLOT_WAIT: Duration = Duration::from_secs(10);
/// Workbooks kept ready, and for how long unused.
const CACHE_ENTRIES: usize = 5;
const CACHE_IDLE: Duration = Duration::from_secs(10 * 60);
/// Stack of a conversion thread (the formula engine recurses).
const STACK: usize = 128 * 1024 * 1024;

/// Item id, saved version, newest live sequence number applied (0: none).
type Key = (String, i64, u64);

struct Loaded {
    book: Book,
    /// Changes whenever the shown content can have: `<version>.<live seq>`.
    rev: String,
}

#[derive(Clone)]
pub(crate) struct Fail {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl Fail {
    fn error(&self) -> ApiError {
        ApiError::coded(self.status, self.code, self.message.clone())
    }
}

type Slot = Arc<OnceCell<Result<Arc<Loaded>, Fail>>>;

/// The workbooks the viewer has open.
pub struct Cache {
    entries: Mutex<Vec<(Key, Slot, Instant)>>,
    convert: Arc<Semaphore>,
}

impl Default for Cache {
    fn default() -> Self {
        Cache { entries: Mutex::new(Vec::new()), convert: Arc::new(Semaphore::new(CONVERSIONS)) }
    }
}

impl Cache {
    fn slot(&self, key: &Key) -> Slot {
        let mut e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        e.retain(|x| now.duration_since(x.2) < CACHE_IDLE);
        if let Some(x) = e.iter_mut().find(|x| &x.0 == key) {
            x.2 = now;
            return x.1.clone();
        }
        // Older versions of the same file are of no use any more.
        e.retain(|x| x.0 .0 != key.0);
        while e.len() >= CACHE_ENTRIES {
            let oldest = e.iter().enumerate().min_by_key(|(_, x)| x.2).map(|(i, _)| i).unwrap_or(0);
            e.remove(oldest);
        }
        let slot: Slot = Arc::default();
        e.push((key.clone(), slot.clone(), now));
        slot
    }
}

/// What the first look at a request settles.
struct Probe {
    item: Item,
    role: Role,
    key: Key,
    link: bool,
    json: Value,
}

fn key_of(s: &Store, it: &Item) -> Result<(Key, Option<crate::store::LiveState>), ApiError> {
    let st = s.live_state(&it.id)?.filter(|st| st.version == it.version);
    let head = if st.is_some() { s.live_head(&it.id)? } else { 0 };
    Ok(((it.id.clone(), it.version, head), st))
}

fn probe(s: &Store, headers: &HeaderMap, id: &str) -> Result<Probe, ApiError> {
    let who = api::auth(s, headers)?;
    let (item, role) = api::visible(s, &who, id, false)?;
    if item.kind != Kind::File {
        return Err(ApiError::bad("Folders can't be opened as a workbook."));
    }
    let (key, _) = key_of(s, &item)?;
    let link = matches!(who, Principal::Link { .. });
    let mut json = api::item_json(s, &item, role);
    if link {
        json["parent"] = Value::Null;
    }
    if let Principal::User { user, .. } = &who {
        // Opening a workbook counts as using it (Recent).
        s.touch_recent(&user.id, &item.id)?;
    }
    Ok(Probe { item, role, key, link, json })
}

/// Everything a conversion needs, read in one consistent step.
pub(crate) struct Inputs {
    pub(crate) key: Key,
    pub(crate) name: String,
    pub(crate) file: std::fs::File,
    pub(crate) live: LiveParts,
}

pub(crate) struct LiveParts {
    pub(crate) state: Option<Vec<u8>>,
    pub(crate) sheets: String,
    pub(crate) batches: Vec<Vec<cosync::Op>>,
}

fn inputs(s: &Store, headers: &HeaderMap, id: &str) -> Result<Inputs, ApiError> {
    let who = api::auth(s, headers)?;
    let (item, _) = api::visible(s, &who, id, false)?;
    let (key, st) = key_of(s, &item)?;
    let (file, _, _) = s.open_content(&item, None)?;
    let mut live = LiveParts { state: None, sheets: String::new(), batches: Vec::new() };
    if let Some(st) = st {
        live.state = s.live_state_blob(&item.id, item.version)?;
        live.sheets = st.sheets.clone();
        for o in s.live_ops_after(&item.id, st.seq)? {
            match serde_json::from_str::<Vec<cosync::Op>>(&o.ops) {
                Ok(ops) => live.batches.push(ops),
                Err(_) => return Err(ApiError::internal()),
            }
        }
    }
    Ok(Inputs { key, name: item.name, file, live })
}

fn fail_of(e: &LoadError) -> Fail {
    let status = match e {
        LoadError::Unsupported(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        LoadError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        LoadError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
    };
    Fail { status, code: e.code(), message: e.message().to_string() }
}

/// Reads and converts the file on a thread of its own, within the limits (see the module docs).
pub(crate) async fn convert(app: &App, i: Inputs) -> Result<Book, Fail> {
    let busy = || Fail { status: StatusCode::SERVICE_UNAVAILABLE, code: "busy", message: "The server is busy opening other workbooks. Try again in a moment.".into() };
    let permit = match tokio::time::timeout(SLOT_WAIT, app.sheets.convert.clone().acquire_owned()).await {
        Ok(Ok(p)) => p,
        _ => return Err(busy()),
    };
    let Inputs { name, file, live, .. } = i;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new().name("sheetview".into()).stack_size(STACK).spawn(move || {
        // Held until the work really ends, even if nobody waits for it any more.
        let _permit = permit;
        let r = (|| {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(MAX_VIEW_BYTES + 1).read_to_end(&mut bytes).map_err(|_| LoadError::Invalid("The file couldn't be read.".into()))?;
            if bytes.len() as u64 > MAX_VIEW_BYTES {
                return Err(LoadError::TooLarge("This file is too large to open in the browser. Download it instead.".into()));
            }
            Book::open(&name, &bytes, &LiveInput { state: live.state, sheets: live.sheets, batches: live.batches })
        })();
        let _ = tx.send(r);
    });
    if spawned.is_err() {
        return Err(busy());
    }
    match tokio::time::timeout(CONVERT_TIMEOUT, rx).await {
        Ok(Ok(Ok(book))) => Ok(book),
        Ok(Ok(Err(e))) => Err(fail_of(&e)),
        Ok(Err(_)) => Err(Fail { status: StatusCode::UNPROCESSABLE_ENTITY, code: "unreadable", message: "This file couldn't be read.".into() }),
        Err(_) => Err(Fail { status: StatusCode::GATEWAY_TIMEOUT, code: "timeout", message: "This workbook is too complex to open in the browser (it took too long). Download it instead.".into() }),
    }
}

/// The open workbook of a request, converting it if nobody has yet.
async fn book(app: &App, headers: &HeaderMap, id: &str) -> Result<(Probe, Arc<Loaded>), ApiError> {
    let (h, i) = (headers.clone(), id.to_string());
    let p = app.run(move |s| probe(s, &h, &i)).await?;
    if p.item.size as u64 > MAX_VIEW_BYTES {
        return Err(ApiError::coded(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "This file is too large to open in the browser. Download it instead."));
    }
    let slot = app.sheets.slot(&p.key);
    let res = slot
        .get_or_init(|| async {
            let (h, i) = (headers.clone(), id.to_string());
            match app.run(move |s| inputs(s, &h, &i)).await {
                Ok(inp) => {
                    let rev = format!("{}.{}", inp.key.1, inp.key.2);
                    convert(app, inp).await.map(|book| Arc::new(Loaded { book, rev }))
                }
                Err(e) => Err(Fail { status: e.status(), code: "error", message: e.message().to_string() }),
            }
        })
        .await;
    match res {
        Ok(l) => Ok((p, l.clone())),
        Err(f) => Err(f.error()),
    }
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    res
}

fn json_ok(v: Value) -> ApiResult {
    Ok(no_store(axum::Json(v).into_response()))
}

fn num(q: &HashMap<String, String>, k: &str) -> Result<u32, ApiError> {
    q.get(k).and_then(|v| v.parse::<u32>().ok()).ok_or_else(|| ApiError::bad(format!("{k} is required (a number).")))
}

pub(crate) async fn meta(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let (p, l) = book(&app, &headers, &id).await?;
    let mut v = l.book.meta();
    v["item"] = p.json;
    v["rev"] = json!(l.rev);
    v["cells"] = json!(l.book.cell_count());
    v["role"] = json!(if p.role >= Role::Edit { "edit" } else { "view" });
    v["live"] = json!(app.hub.connected(&p.item.id));
    v["link"] = json!(p.link);
    json_ok(v)
}

pub(crate) async fn cells(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let sheet = num(&q, "sheet")? as usize;
    let (r1, r2, c1, c2) = (num(&q, "r1")?, num(&q, "r2")?, num(&q, "c1")?, num(&q, "c2")?);
    let (_, l) = book(&app, &headers, &id).await?;
    let w = tokio::task::spawn_blocking(move || {
        let mut v = l.book.window_at(sheet, r1, c1, r2, c2)?;
        v["rev"] = json!(l.rev);
        Ok::<_, String>(v)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::bad)?;
    json_ok(w)
}

pub(crate) async fn cell(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let (sheet, r, c) = (num(&q, "sheet")? as usize, num(&q, "r")?, num(&q, "c")?);
    let (_, l) = book(&app, &headers, &id).await?;
    let mut v = l.book.cell(sheet, r, c).map_err(ApiError::bad)?;
    v["rev"] = json!(l.rev);
    json_ok(v)
}

/// One picture of a sheet, as a browser can show it (png, jpeg, gif, bmp), never as a page.
pub(crate) async fn image(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let (sheet, n) = (num(&q, "sheet")? as usize, num(&q, "n")? as usize);
    let (_, l) = book(&app, &headers, &id).await?;
    let (ext, data) = l.book.image(sheet, n).ok_or_else(ApiError::not_found)?;
    let mut res = data.as_ref().clone().into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&format!("image/{ext}")).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'"));
    Ok(no_store(res))
}

/// The charts of a sheet with their data (for viewers that aren't in a live session).
pub(crate) async fn charts(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let sheet = num(&q, "sheet")? as usize;
    let (_, l) = book(&app, &headers, &id).await?;
    let v = tokio::task::spawn_blocking(move || l.book.charts(sheet)).await.map_err(|_| ApiError::internal())?.map_err(ApiError::bad)?;
    json_ok(v)
}

pub(crate) async fn csv(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let sheet = num(&q, "sheet")? as usize;
    let (p, l) = book(&app, &headers, &id).await?;
    let text = tokio::task::spawn_blocking(move || l.book.csv(sheet)).await.map_err(|_| ApiError::internal())?.map_err(ApiError::bad)?;
    let name = format!("{}.csv", p.item.name.rsplit_once('.').map_or(p.item.name.as_str(), |(s, _)| s));
    let mut res = text.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    h.insert(header::CONTENT_DISPOSITION, crate::content::disposition(&name, true));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'"));
    Ok(no_store(res))
}
