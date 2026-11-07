//! Web Sheets, live editing: `GET /api/items/{id}/web-live?ticket=<t>` upgrades to a WebSocket
//! that speaks a small JSON protocol with the browser (see `sheetview::bridge`). For each such
//! connection the server runs a headless live client (a `Bridge`: the workbook, the OT session
//! and the formula engine) that joins the document's live session in this process, through the
//! same `live::run` every other participant uses, over an in-memory socket. So permissions,
//! live role changes and revocation, presence, rate limits, snapshots into versions and the
//! merge rules are exactly those of the desktop app; formulas and OT stay in Rust.
//!
//! The browser authenticates like for `/live`: the web cookie never opens a WebSocket; it asks
//! `POST /api/items/{id}/live-ticket` (needs the CSRF header) and passes the one-minute,
//! single-use ticket.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use cosync::{ClientMsg, ServerMsg};
use futures_util::{Sink, Stream, StreamExt};
use serde_json::{json, Value};
use sheetview::bridge::Bridge;
use sheetview::Book;
use tokio::sync::mpsc;

use crate::api::{self, ApiError, ApiResult, App};
use crate::store::{Item, Kind, Principal, Role};
use crate::util;

/// A browser message longer than this closes the connection.
const MAX_BROWSER_MESSAGE: usize = 256 * 1024;
const TICK: Duration = Duration::from_millis(400);

/// One end of an in-memory "WebSocket" (what `live::run` needs: messages in and out).
struct Mem {
    rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
}

fn pair() -> (Mem, Mem) {
    let (a_tx, a_rx) = mpsc::unbounded_channel();
    let (b_tx, b_rx) = mpsc::unbounded_channel();
    (Mem { rx: a_rx, tx: b_tx }, Mem { rx: b_rx, tx: a_tx })
}

impl Stream for Mem {
    type Item = Result<Message, axum::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|o| o.map(Ok))
    }
}

impl Sink<Message> for Mem {
    type Error = axum::Error;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.tx.send(item).map_err(|_| axum::Error::new("closed"))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

pub(crate) async fn handler(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> ApiResult {
    // Only a ticket (from a request that passed the CSRF check) opens this, never the cookie.
    let ticket = q.get("ticket").cloned().ok_or_else(ApiError::unauthorized)?;
    let hub = app.hub.clone();
    let iid = id.clone();
    let (item, who, role) = app
        .run(move |s| {
            let issued = hub.redeem(&ticket, &iid).ok_or_else(ApiError::unauthorized)?;
            let who = s.recheck_principal(&issued)?.map_err(|_| ApiError::unauthorized())?;
            let (it, role) = api::visible(s, &who, &iid, false)?;
            if it.kind != Kind::File {
                return Err(ApiError::bad("Folders can't be edited live."));
            }
            Ok((it, who, role))
        })
        .await?;
    let Ok(ws) = ws else {
        return Err(ApiError::new(StatusCode::UPGRADE_REQUIRED, "Live editing needs a WebSocket connection."));
    };
    Ok(ws.max_message_size(MAX_BROWSER_MESSAGE).max_frame_size(MAX_BROWSER_MESSAGE).on_upgrade(move |socket| run(app, socket, item, who, role)))
}

async fn say(browser: &mut WebSocket, v: Value) -> bool {
    browser.send(Message::Text(v.to_string().into())).await.is_ok()
}

async fn fail(mut browser: WebSocket, code: &str, message: &str) {
    let _ = say(&mut browser, json!({ "t": "failed", "code": code, "message": message })).await;
    let _ = browser.send(Message::Close(Some(CloseFrame { code: 1000, reason: "".into() }))).await;
}

async fn run(app: App, mut browser: WebSocket, item: Item, who: Principal, role: Role) {
    // The base: the saved version (with the lossless live state when it has one), where the log starts.
    let iid = item.id.clone();
    let base = app
        .run(move |s| {
            let it = s.item(&iid)?.ok_or_else(ApiError::not_found)?;
            let base = s.live_base(&it)?;
            let (file, _, _) = s.open_content(&it, None)?;
            let state = if base.version == it.version { s.live_state_blob(&it.id, it.version)? } else { None };
            Ok((it, base, file, state))
        })
        .await;
    let (it, base, file, state) = match base {
        Ok(b) => b,
        Err(e) => return fail(browser, "error", e.message()).await,
    };
    if it.size as u64 > crate::sheetweb::MAX_VIEW_BYTES {
        return fail(browser, "too_large", "This file is too large to open in the browser. Download it instead.").await;
    }
    let inputs = crate::sheetweb::Inputs {
        key: (it.id.clone(), it.version, base.seq),
        name: it.name.clone(),
        file,
        live: crate::sheetweb::LiveParts { state, sheets: base.sheets.clone(), batches: Vec::new() },
    };
    let book: Book = match crate::sheetweb::convert(&app, inputs).await {
        Ok(b) => b,
        Err(f) => return fail(browser, f.code, &f.message).await,
    };
    let cid = format!("web-{}", &util::new_id()[..12]);
    let mut bridge = Bridge::new(book, base.seq, cid.clone());

    // Join the live session in this process.
    let (mut hub_end, server_end) = pair();
    let join = crate::live::Join { actor: api::actor(&who), item: it.clone(), version: Some(it.version), who: who.clone(), role, since: base.seq, cid, agent: false };
    let (store, hub) = (app.store.clone(), app.hub.clone());
    let live_task = tokio::spawn(crate::live::run(store, hub, server_end, join));
    let hello = serde_json::to_string(&bridge.hello()).unwrap_or_default();
    let _ = hub_end.tx.send(Message::Text(hello.into()));

    let mut tick = tokio::time::interval(TICK);
    let mut snapshotting = false;
    let (snap_tx, mut snap_rx) = mpsc::unbounded_channel::<()>();
    'conn: loop {
        let out = tokio::select! {
            msg = browser.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Text(t) => match serde_json::from_str::<Value>(&t) {
                        Ok(v) => bridge.on_browser(&v),
                        Err(_) => break,
                    },
                    Message::Close(_) => break,
                    Message::Binary(_) => break,
                    _ => continue,
                }
            }
            msg = hub_end.rx.recv() => {
                match msg {
                    Some(Message::Text(t)) => match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(m) => bridge.on_server(m),
                        Err(_) => break,
                    },
                    Some(Message::Close(f)) => {
                        let code = f.map(|f| f.code).unwrap_or(1000);
                        let mut o = bridge.lost();
                        o.browser.push(json!({ "t": "closed", "code": code }));
                        deliver(&mut browser, &hub_end, o).await;
                        break;
                    }
                    Some(_) => continue,
                    None => {
                        let mut o = bridge.lost();
                        o.browser.push(json!({ "t": "closed", "code": 1011 }));
                        deliver(&mut browser, &hub_end, o).await;
                        break;
                    }
                }
            }
            _ = tick.tick() => {
                if !snapshotting && let Some(job) = bridge.snapshot_job() {
                    snapshotting = true;
                    let (app, item, who, tx) = (app.clone(), it.clone(), who.clone(), snap_tx.clone());
                    tokio::spawn(async move {
                        store_snapshot(&app, &item, &who, job).await;
                        let _ = tx.send(());
                    });
                }
                continue;
            }
            Some(()) = snap_rx.recv() => {
                snapshotting = false;
                continue;
            }
        };
        if !deliver(&mut browser, &hub_end, out).await {
            break 'conn;
        }
        if bridge.stopped.is_some() {
            break;
        }
    }
    drop(hub_end);
    let _ = tokio::time::timeout(Duration::from_secs(5), live_task).await;
    let _ = browser.send(Message::Close(None)).await;
}

/// Sends what a step produced; false when the browser is gone.
async fn deliver(browser: &mut WebSocket, hub: &Mem, out: sheetview::bridge::Out) -> bool {
    for m in out.server {
        let text = serde_json::to_string::<ClientMsg>(&m).unwrap_or_default();
        let _ = hub.tx.send(Message::Text(text.into()));
    }
    for v in out.browser {
        if !say(browser, v).await {
            return false;
        }
    }
    true
}

/// Stores the workbook as the next version, as the app does when the server asks.
async fn store_snapshot(app: &App, item: &Item, who: &Principal, job: sheetview::bridge::SnapshotJob) {
    let seq = job.seq;
    let encoded = tokio::task::spawn_blocking(move || job.encode()).await;
    let Ok(Ok((data, state, sheets))) = encoded else { return };
    if data.is_empty() || data.len() > app.cfg().max_upload_bytes() {
        return;
    }
    let (id, w) = (item.id.clone(), who.clone());
    let stored = app
        .run(move |s| {
            let (it, role) = api::visible(s, &w, &id, false)?;
            api::need_edit(role)?;
            let (saved, st) = s.live_snapshot(&it, &data, &state, &api::actor(&w), seq, &sheets)?;
            if saved.version != it.version {
                api::audit_version(s, &w, &saved, "live")?;
            }
            Ok(st)
        })
        .await;
    if let Ok(st) = stored {
        app.hub.snapshotted(&item.id, st.seq, st.floor);
    }
}
