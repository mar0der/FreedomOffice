//! Server configuration (`/etc/freedomoffice/server.toml`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = "/etc/freedomoffice/server.toml";
pub const DEFAULT_DATA_DIR: &str = "/var/lib/freedomoffice";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Address and port to listen on, e.g. "127.0.0.1:8080" or "0.0.0.0:443".
    pub listen: String,
    pub data_dir: PathBuf,
    /// Base URL clients use, e.g. "https://files.example.com" (used in share links).
    pub public_url: String,
    /// Domain for automatic Let's Encrypt certificates (TLS-ALPN-01 on the listen port, normally 443).
    pub tls_domain: Option<String>,
    /// Contact e-mail for the Let's Encrypt account.
    pub acme_email: Option<String>,
    /// Use the Let's Encrypt staging directory (for trying out a setup).
    pub acme_staging: bool,
    /// PEM certificate chain and private key (instead of Let's Encrypt).
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Plain HTTP on a non-loopback address must be allowed explicitly.
    pub allow_insecure_http: bool,
    /// Take the client address from X-Forwarded-For when the peer is a loopback reverse proxy.
    pub trust_proxy: bool,
    pub max_upload_mb: u64,
    /// Versions kept per file (the first version of each day is kept as well).
    pub versions_to_keep: u32,
    /// Lifetime of the access tokens handed out for share links.
    pub link_access_hours: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "127.0.0.1:8080".into(),
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            public_url: String::new(),
            tls_domain: None,
            acme_email: None,
            acme_staging: false,
            tls_cert: None,
            tls_key: None,
            allow_insecure_http: false,
            trust_proxy: true,
            max_upload_mb: 100,
            versions_to_keep: 25,
            link_access_hours: 12,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Tls {
    None,
    Files { cert: PathBuf, key: PathBuf },
    Acme { domain: String },
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
        Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let c: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.listen_addr()?;
        self.tls()?;
        if self.max_upload_mb == 0 {
            return Err("max_upload_mb must be at least 1".into());
        }
        if self.versions_to_keep == 0 {
            return Err("versions_to_keep must be at least 1".into());
        }
        if !self.public_url.is_empty() && !(self.public_url.starts_with("https://") || self.public_url.starts_with("http://")) {
            return Err("public_url must start with https://".into());
        }
        Ok(())
    }

    pub fn listen_addr(&self) -> Result<SocketAddr, String> {
        self.listen.parse().map_err(|_| format!("listen = \"{}\" is not an address like 127.0.0.1:8080", self.listen))
    }

    pub fn tls(&self) -> Result<Tls, String> {
        match (&self.tls_cert, &self.tls_key, &self.tls_domain) {
            (Some(c), Some(k), _) => Ok(Tls::Files { cert: c.clone(), key: k.clone() }),
            (Some(_), None, _) | (None, Some(_), _) => Err("tls_cert and tls_key must be set together".into()),
            (None, None, Some(d)) if !d.trim().is_empty() => Ok(Tls::Acme { domain: d.trim().to_string() }),
            _ => Ok(Tls::None),
        }
    }

    /// Plain HTTP is only served on loopback (behind a reverse proxy) unless explicitly allowed.
    pub fn check_plain_http(&self) -> Result<(), String> {
        let addr = self.listen_addr()?;
        if self.tls()? == Tls::None && !addr.ip().is_loopback() && !self.allow_insecure_http {
            return Err(format!(
                "Refusing to serve plain HTTP on {addr}: passwords and documents would cross the network unencrypted. \
                 Set tls_domain (Let's Encrypt) or tls_cert/tls_key, listen on 127.0.0.1 behind a reverse proxy, \
                 or set allow_insecure_http = true."
            ));
        }
        Ok(())
    }

    /// Base for share links: `public_url`, else derived from the TLS domain or listen address.
    pub fn base_url(&self) -> String {
        if !self.public_url.is_empty() {
            return self.public_url.trim_end_matches('/').to_string();
        }
        match self.tls() {
            Ok(Tls::Acme { domain }) => format!("https://{domain}"),
            _ => format!("http://{}", self.listen),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("freedomoffice.db")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
    }

    pub fn max_upload_bytes(&self) -> usize {
        (self.max_upload_mb as usize).saturating_mul(1024 * 1024)
    }
}

/// The commented configuration written by `init`.
pub fn template(listen: &str, data_dir: &Path, public_url: &str, tls_domain: Option<&str>, acme_email: Option<&str>) -> String {
    let domain_line = match tls_domain {
        Some(d) => format!("tls_domain = \"{d}\""),
        None => "# tls_domain = \"files.example.com\"".into(),
    };
    let email_line = match acme_email {
        Some(e) => format!("acme_email = \"{e}\""),
        None => "# acme_email = \"admin@example.com\"".into(),
    };
    format!(
        r#"# FreedomOffice document server configuration.

# Address and port. Use "127.0.0.1:8080" behind a reverse proxy (Caddy, nginx),
# or "0.0.0.0:443" / "[::]:443" with tls_domain for built-in HTTPS.
listen = "{listen}"

# Database and documents. Back this directory up (see `freedomoffice-server backup`).
data_dir = "{}"

# The address people use to reach the server; used in share links.
public_url = "{public_url}"

# Built-in HTTPS with automatic Let's Encrypt certificates (the listen port must be 443
# and reachable from the internet; the domain's DNS A/AAAA record must point here).
{domain_line}
{email_line}
# acme_staging = true      # try the setup against the Let's Encrypt staging directory first

# Or use your own certificate files instead of Let's Encrypt:
# tls_cert = "/etc/freedomoffice/fullchain.pem"
# tls_key = "/etc/freedomoffice/privkey.pem"

# Plain HTTP is only served on loopback unless this is set (never do this on the internet).
allow_insecure_http = false

# Behind a reverse proxy on this machine, read the client address from X-Forwarded-For
# (used for sign-in rate limiting).
trust_proxy = true

# Largest document upload in megabytes.
max_upload_mb = 100

# Versions kept per document; the first version of each day is kept in addition.
versions_to_keep = 25

# How long a share link stays usable after it was opened, in hours.
link_access_hours = 12
"#,
        data_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_plain_http_rules() {
        let t = template("127.0.0.1:8080", Path::new("/var/lib/freedomoffice"), "https://files.example.com", None, None);
        let c = Config::parse(&t).unwrap();
        assert_eq!(c.tls().unwrap(), Tls::None);
        assert!(c.check_plain_http().is_ok());
        let c = Config::parse("listen = \"0.0.0.0:8080\"").unwrap();
        assert!(c.check_plain_http().is_err());
        let c = Config::parse("listen = \"0.0.0.0:8080\"\nallow_insecure_http = true").unwrap();
        assert!(c.check_plain_http().is_ok());
        let c = Config::parse("listen = \"0.0.0.0:443\"\ntls_domain = \"x.example.com\"").unwrap();
        assert_eq!(c.tls().unwrap(), Tls::Acme { domain: "x.example.com".into() });
        assert!(c.check_plain_http().is_ok());
        assert_eq!(c.base_url(), "https://x.example.com");
        assert!(Config::parse("tls_cert = \"/a\"").is_err());
        assert!(Config::parse("bogus = 1").is_err());
        assert!(Config::parse("listen = \"nope\"").is_err());
    }
}
