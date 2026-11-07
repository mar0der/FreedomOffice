//! The live connection: a WebSocket (wss://, or ws:// for localhost) on a background thread,
//! talking to the app over channels. No async runtime: tungstenite on a non-blocking socket.

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cosync::{ClientMsg, ServerMsg};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// What the connection thread reports.
#[derive(Debug)]
pub enum Event {
    Open,
    Msg(ServerMsg),
    /// The connection ended (or never opened). `code`: the server's close code; `http`: the
    /// HTTP status when the handshake was refused.
    Closed { code: Option<u16>, http: Option<u16>, reason: String },
}

/// Where to connect.
#[derive(Clone, Debug)]
pub struct Target {
    /// Server base URL (https://host or http://localhost:port).
    pub url: String,
    pub token: String,
    pub item: String,
    pub since: u64,
    /// The version a first join downloaded (None when resuming).
    pub version: Option<i64>,
    pub cid: String,
}

pub struct Conn {
    tx: Sender<ClientMsg>,
    pub rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
}

impl Conn {
    pub fn send(&self, m: ClientMsg) {
        let _ = self.tx.send(m);
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The WebSocket URL of a document's live session.
pub fn ws_url(t: &Target) -> String {
    let base = if let Some(rest) = t.url.strip_prefix("https://") { format!("wss://{rest}") } else { t.url.replacen("http://", "ws://", 1) };
    let mut u = format!("{base}/api/items/{}/live?since={}&cid={}&proto={}", crate::cloud::pct_encode(&t.item), t.since, crate::cloud::pct_encode(&t.cid), cosync::PROTO);
    if let Some(v) = t.version {
        u.push_str(&format!("&v={v}"));
    }
    u
}

/// Starts connecting; events arrive on `rx` (and wake the UI through `wake`).
pub fn connect(t: Target, wake: Option<eframe::egui::Context>) -> Conn {
    let (tx, out_rx) = channel::<ClientMsg>();
    let (ev_tx, rx) = channel::<Event>();
    let stop = Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    std::thread::spawn(move || {
        let send = |e: Event| {
            let _ = ev_tx.send(e);
            if let Some(c) = &wake {
                c.request_repaint();
            }
        };
        let end = run(&t, &out_rx, &st, &send);
        if !st.load(Ordering::SeqCst) {
            send(end);
        }
    });
    Conn { tx, rx, stop }
}

fn set_nonblocking(ws: &WebSocket<MaybeTlsStream<TcpStream>>) -> std::io::Result<()> {
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s.set_nonblocking(true),
        MaybeTlsStream::NativeTls(s) => s.get_ref().set_nonblocking(true),
        _ => Ok(()),
    }
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
}

fn run(t: &Target, out: &Receiver<ClientMsg>, stop: &AtomicBool, send: &dyn Fn(Event)) -> Event {
    let closed = |reason: String| Event::Closed { code: None, http: None, reason };
    let mut req = match ws_url(t).into_client_request() {
        Ok(r) => r,
        Err(e) => return closed(e.to_string()),
    };
    match format!("Bearer {}", t.token).parse() {
        Ok(v) => {
            req.headers_mut().insert("Authorization", v);
        }
        Err(_) => return closed("Invalid sign-in token.".into()),
    }
    let mut ws = match tungstenite::connect(req) {
        Ok((ws, _)) => ws,
        Err(tungstenite::Error::Http(r)) => return Event::Closed { code: None, http: Some(r.status().as_u16()), reason: format!("HTTP {}", r.status().as_u16()) },
        Err(e) => return closed(format!("Couldn't reach the server ({e})")),
    };
    if let Err(e) = set_nonblocking(&ws) {
        return closed(e.to_string());
    }
    send(Event::Open);
    let mut last_in = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = ws.close(None);
            let _ = ws.flush();
            return closed("stopped".into());
        }
        // Outgoing: wait briefly for something to send (this is also the poll interval).
        match out.recv_timeout(Duration::from_millis(15)) {
            Ok(m) => {
                let mut batch = vec![m];
                while let Ok(m) = out.try_recv() {
                    batch.push(m);
                }
                for m in batch {
                    let text = serde_json::to_string(&m).unwrap_or_default();
                    match ws.write(Message::Text(text.into())) {
                        Ok(()) => {}
                        Err(e) if would_block(&e) => {}
                        Err(e) => return closed(format!("Connection lost ({e})")),
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let _ = ws.close(None);
                let _ = ws.flush();
                return closed("closed".into());
            }
        }
        match ws.flush() {
            Ok(()) => {}
            Err(e) if would_block(&e) => {}
            Err(e) => return closed(format!("Connection lost ({e})")),
        }
        // Incoming.
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    last_in = Instant::now();
                    match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(m) => send(Event::Msg(m)),
                        Err(_) => return closed("The server sent something unexpected.".into()),
                    }
                }
                Ok(Message::Close(f)) => {
                    let _ = ws.flush();
                    return Event::Closed { code: f.as_ref().map(|f| f.code.into()), http: None, reason: f.map(|f| f.reason.to_string()).unwrap_or_default() };
                }
                Ok(_) => last_in = Instant::now(),
                Err(e) if would_block(&e) => break,
                Err(e) => return closed(format!("Connection lost ({e})")),
            }
        }
        // The server pings every 20 seconds; a minute of silence means the link is dead.
        if last_in.elapsed() > Duration::from_secs(60) {
            return closed("The server stopped answering.".into());
        }
    }
}
