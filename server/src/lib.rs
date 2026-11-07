//! FreedomOffice document server: accounts, document storage with version history,
//! sharing and share links for FreedomOffice Sheets.

pub mod admin;
pub mod api;
pub mod config;
pub mod content;
pub mod live;
pub mod store;
pub mod tls;
pub mod util;
pub mod web;

use std::net::SocketAddr;

use config::{Config, Tls};
use store::Store;

pub fn open_store(cfg: &Config) -> Result<Store, String> {
    Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).map_err(|e| format!("Couldn't open the data directory {}: {e}", cfg.data_dir.display()))
}

/// Serves until Ctrl+C / SIGTERM.
pub async fn serve(cfg: Config) -> Result<(), String> {
    cfg.check_plain_http()?;
    let addr = cfg.listen_addr()?;
    let tcp = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("Couldn't listen on {addr}: {e}"))?;
    serve_on(cfg, tcp, shutdown_signal()).await
}

pub async fn serve_on(cfg: Config, tcp: tokio::net::TcpListener, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<(), String> {
    let store = open_store(&cfg)?;
    let tls = cfg.tls()?;
    let local = tcp.local_addr().map_err(|e| e.to_string())?;
    let app = api::router(api::App::new(store, cfg.clone()));
    let svc = app.into_make_service_with_connect_info::<api::Peer>();
    match tls {
        Tls::None => {
            tracing::info!("listening on http://{local}");
            axum::serve(tcp, svc).with_graceful_shutdown(shutdown).await.map_err(|e| e.to_string())
        }
        Tls::Files { cert, key } => {
            let l = tls::with_files(tcp, &cert, &key).await?;
            tracing::info!("listening on https://{local} (certificate from {})", cert.display());
            axum::serve(l, svc).with_graceful_shutdown(shutdown).await.map_err(|e| e.to_string())
        }
        Tls::Acme { domain } => {
            let cache = cfg.data_dir.join("acme");
            std::fs::create_dir_all(&cache).map_err(|e| format!("Couldn't create {}: {e}", cache.display()))?;
            let l = tls::with_acme(tcp, &domain, cfg.acme_email.as_deref(), cfg.acme_staging, &cache).await?;
            tracing::info!("listening on https://{domain} ({local}, Let's Encrypt{})", if cfg.acme_staging { " staging" } else { "" });
            axum::serve(l, svc).with_graceful_shutdown(shutdown).await.map_err(|e| e.to_string())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}

/// A server on 127.0.0.1 (random port) running on its own thread, for tests.
pub struct Running {
    pub addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Running {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn spawn(mut cfg: Config) -> Result<Running, String> {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    std_listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let addr = std_listener.local_addr().map_err(|e| e.to_string())?;
    cfg.listen = addr.to_string();
    if cfg.public_url.is_empty() {
        cfg.public_url = format!("http://{addr}");
    }
    // Open once here so setup errors surface to the caller.
    drop(open_store(&cfg)?);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
        rt.block_on(async move {
            let tcp = tokio::net::TcpListener::from_std(std_listener).expect("listener");
            let _ = serve_on(cfg, tcp, async move {
                let _ = stop_rx.await;
            })
            .await;
        });
    });
    Ok(Running { addr, stop: Some(stop_tx), thread: Some(thread) })
}
