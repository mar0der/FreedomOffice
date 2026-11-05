//! HTTPS: a listener that hands finished TLS connections to axum. Certificates come from PEM
//! files or from Let's Encrypt (TLS-ALPN-01 on the same port, handled by rustls-acme).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rustls_acme::caches::DirCache;
use rustls_acme::{is_tls_alpn_challenge, AcmeConfig};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::server::TlsStream;
use tokio_rustls::{LazyConfigAcceptor, TlsAcceptor};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(c) => c,
            // The accept loop never ends while the listener lives.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local)
    }
}

fn provider() -> Arc<tokio_rustls::rustls::crypto::CryptoProvider> {
    Arc::new(tokio_rustls::rustls::crypto::ring::default_provider())
}

fn alpn(mut c: ServerConfig) -> Arc<ServerConfig> {
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(c)
}

/// HTTPS with a certificate chain and key from PEM files.
pub async fn with_files(tcp: TcpListener, cert: &Path, key: &Path) -> Result<TlsListener, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("Couldn't read {}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("Couldn't read {}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!("{} contains no certificates", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("Couldn't read {}: {e}", key.display()))?;
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("The certificate and key don't match: {e}"))?;
    let acceptor = TlsAcceptor::from(alpn(config));
    let local = tcp.local_addr().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        loop {
            let Ok((stream, peer)) = tcp.accept().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            let (acceptor, tx) = (acceptor.clone(), tx.clone());
            tokio::spawn(async move {
                if let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                    let _ = tx.send((tls, peer)).await;
                }
            });
        }
    });
    Ok(TlsListener { rx, local })
}

/// HTTPS with automatic Let's Encrypt certificates for `domain`, cached in `cache_dir`.
pub async fn with_acme(tcp: TcpListener, domain: &str, email: Option<&str>, staging: bool, cache_dir: &Path) -> Result<TlsListener, String> {
    let mut cfg = AcmeConfig::new([domain]).cache(DirCache::new(cache_dir.to_path_buf())).directory_lets_encrypt(!staging);
    if let Some(e) = email.filter(|e| !e.trim().is_empty()) {
        cfg = cfg.contact_push(format!("mailto:{}", e.trim()));
    }
    let mut state = cfg.state();
    let challenge = state.challenge_rustls_config();
    let default = {
        let c = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_cert_resolver(state.resolver());
        alpn(c)
    };
    tokio::spawn(async move {
        while let Some(ev) = state.next().await {
            match ev {
                Ok(ok) => tracing::info!("certificate: {ok:?}"),
                Err(err) => tracing::error!("certificate error: {err:?}"),
            }
        }
    });
    let local = tcp.local_addr().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        loop {
            let Ok((stream, peer)) = tcp.accept().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            let (challenge, default, tx) = (challenge.clone(), default.clone(), tx.clone());
            tokio::spawn(async move {
                let work = async {
                    let start = LazyConfigAcceptor::new(Default::default(), stream).await.ok()?;
                    if is_tls_alpn_challenge(&start.client_hello()) {
                        tracing::info!("answering a Let's Encrypt TLS-ALPN-01 validation");
                        let mut tls = start.into_stream(challenge).await.ok()?;
                        let _ = tokio::io::AsyncWriteExt::shutdown(&mut tls).await;
                        None
                    } else {
                        start.into_stream(default).await.ok()
                    }
                };
                if let Ok(Some(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, work).await {
                    let _ = tx.send((tls, peer)).await;
                }
            });
        }
    });
    Ok(TlsListener { rx, local })
}
