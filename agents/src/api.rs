//! The server's HTTP API, as much of it as an agent needs: sign in, list, download, upload.
//! Blocking (ureq). Passwords are sent only to `/api/login`; afterwards only the device token.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

pub struct Resp {
    pub status: u16,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn error(&self) -> String {
        let v = self.json();
        match v["error"].as_str() {
            Some(e) => format!("{e} (HTTP {})", self.status),
            None => format!("HTTP {}", self.status),
        }
    }
}

/// A file or folder as the server lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub folder: bool,
    pub version: i64,
    pub role: String,
    pub size: u64,
}

impl Item {
    pub fn from_json(v: &Value) -> Item {
        Item {
            id: v["id"].as_str().unwrap_or("").to_string(),
            name: v["name"].as_str().unwrap_or("").to_string(),
            folder: v["kind"] == "folder",
            version: v["version"].as_i64().unwrap_or(0),
            role: v["role"].as_str().unwrap_or("").to_string(),
            size: v["size"].as_u64().unwrap_or(0),
        }
    }
}

#[derive(Clone)]
pub struct Api {
    pub url: String,
    pub token: String,
    pub username: String,
    agent: ureq::Agent,
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// "http://host:port" without a trailing slash.
pub fn normalize_url(u: &str) -> String {
    let u = u.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

pub fn pct_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Api {
    /// Signs in; the password is used for this one request only.
    pub fn login(url: &str, username: &str, password: &str, device: &str) -> Result<Api, String> {
        let url = normalize_url(url);
        let mut api = Api {
            url,
            token: String::new(),
            username: username.to_string(),
            agent: agent(),
        };
        let r = api.send(
            "POST",
            "/api/login",
            &[("Content-Type", "application/json")],
            Some(
                json!({ "username": username, "password": password, "device": device })
                    .to_string()
                    .into_bytes(),
            ),
        )?;
        if r.status == 401 {
            return Err("The username or password is incorrect.".into());
        }
        if !r.ok() {
            return Err(r.error());
        }
        let v = r.json();
        api.token = v["token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or("That address didn't answer like a FreedomOffice server.")?
            .to_string();
        api.username = v["user"]["username"]
            .as_str()
            .unwrap_or(username)
            .to_string();
        if v["user"]["must_change_password"].as_bool().unwrap_or(false) {
            return Err(
                "This account must choose a new password first (sign in with the app).".into(),
            );
        }
        Ok(api)
    }

    pub fn with_token(url: &str, username: &str, token: &str) -> Api {
        Api {
            url: normalize_url(url),
            token: token.trim().to_string(),
            username: username.to_string(),
            agent: agent(),
        }
    }

    /// Reads a token file written by `save_token`: line 1 the server URL, 2 the username, 3 the token.
    pub fn from_token_file(path: &Path) -> Result<Api, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
        let mut l = text.lines();
        match (l.next(), l.next(), l.next()) {
            (Some(u), Some(n), Some(t)) if !t.is_empty() => Ok(Api::with_token(u, n, t)),
            _ => Err(format!("{} isn't a token file.", path.display())),
        }
    }

    /// Writes the token to a file only the owner can read.
    pub fn save_token(&self, path: &Path) -> Result<(), String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("Couldn't write {}: {e}", path.display()))?;
        writeln!(f, "{}\n{}\n{}", self.url, self.username, self.token).map_err(|e| e.to_string())
    }

    pub fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<Resp, String> {
        let mut b = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.url));
        if !self.token.is_empty() {
            b = b.header("Authorization", format!("Bearer {}", self.token));
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let res = match body {
            Some(data) => self.agent.run(b.body(data).map_err(|e| e.to_string())?),
            None => self.agent.run(b.body(()).map_err(|e| e.to_string())?),
        };
        let mut res = res.map_err(|e| format!("Couldn't reach {}: {e}", self.url))?;
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = res
            .body_mut()
            .with_config()
            .limit(256 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        Ok(Resp {
            status,
            body,
            headers,
        })
    }

    fn get_json(&self, path: &str) -> Result<Value, String> {
        let r = self.send("GET", path, &[], None)?;
        if !r.ok() {
            return Err(r.error());
        }
        Ok(r.json())
    }

    /// `/api/me`: the person, their organisations and the like.
    pub fn me(&self) -> Result<Value, String> {
        self.get_json("/api/me")
    }

    /// The root folder id of "My files".
    pub fn root(&self) -> Result<String, String> {
        Ok(self.get_json("/api/root")?["id"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    pub fn item(&self, id: &str) -> Result<Item, String> {
        Ok(Item::from_json(
            &self.get_json(&format!("/api/items/{}", pct_encode(id)))?,
        ))
    }

    pub fn children(&self, folder: &str) -> Result<Vec<Item>, String> {
        let v = self.get_json(&format!("/api/items/{}/children", pct_encode(folder)))?;
        Ok(v["items"]
            .as_array()
            .map(|a| a.iter().map(Item::from_json).collect())
            .unwrap_or_default())
    }

    /// The organisation folders of the signed-in person: (org name, root folder id).
    pub fn org_roots(&self) -> Result<Vec<(String, String)>, String> {
        let me = self.me()?;
        Ok(me["orgs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|o| {
                        (
                            o["name"].as_str().unwrap_or("").to_string(),
                            o["root"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The current content of a file with the headers that say where a live session starts.
    pub fn content(&self, id: &str) -> Result<Resp, String> {
        let r = self.send(
            "GET",
            &format!("/api/items/{}/content", pct_encode(id)),
            &[],
            None,
        )?;
        if !r.ok() {
            return Err(r.error());
        }
        Ok(r)
    }

    /// Uploads a new file into a folder (fails when the name is taken).
    pub fn upload_new(&self, folder: &str, name: &str, data: Vec<u8>) -> Result<Item, String> {
        let r = self.send(
            "POST",
            &format!(
                "/api/files?parent={}&name={}&conflict=fail",
                pct_encode(folder),
                pct_encode(name)
            ),
            &[],
            Some(data),
        )?;
        if !r.ok() {
            return Err(r.error());
        }
        Ok(Item::from_json(&r.json()))
    }

    /// The WebSocket URL of an item's live session.
    pub fn live_url(
        &self,
        id: &str,
        since: u64,
        version: Option<i64>,
        cid: &str,
        agent: bool,
    ) -> String {
        let base = if let Some(rest) = self.url.strip_prefix("https://") {
            format!("wss://{rest}")
        } else {
            self.url.replacen("http://", "ws://", 1)
        };
        let mut u = format!(
            "{base}/api/items/{}/live?since={since}&cid={}",
            pct_encode(id),
            pct_encode(cid)
        );
        if let Some(v) = version {
            u.push_str(&format!("&v={v}"));
        }
        if agent {
            u.push_str("&agent=1");
        }
        u
    }
}
