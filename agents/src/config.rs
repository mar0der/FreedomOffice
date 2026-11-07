//! How an agent signs in without a password ever being an argument: a password file (mode
//! 0600) read once to sign in, or a cached device token (mode 0600) under
//! `~/.config/freedomsheets-agents/`. Secrets are never printed or logged.

use std::path::{Path, PathBuf};

use crate::api::Api;

/// What the caller says about the account; every field is optional when a token file exists.
#[derive(Clone, Debug, Default)]
pub struct Account {
    pub server: Option<String>,
    pub user: Option<String>,
    /// A file with the password on its first line (mode 0600).
    pub password_file: Option<PathBuf>,
    /// A token file written by `fsheets login` (mode 0600). Defaults to the cache directory.
    pub token_file: Option<PathBuf>,
}

/// `$XDG_CONFIG_HOME/freedomsheets-agents` or `~/.config/freedomsheets-agents`.
pub fn config_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(std::env::var_os("HOME").filter(|v| !v.is_empty())?).join(".config"),
    };
    Some(base.join("freedomsheets-agents"))
}

/// The cached token of an account: `<user>@<host>.token` in the config directory.
pub fn token_path(server: &str, user: &str) -> Option<PathBuf> {
    let host = crate::api::normalize_url(server)
        .split_once("://")
        .map(|(_, h)| h.to_string())
        .unwrap_or_default();
    let clean = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    Some(config_dir()?.join(format!("{}@{}.token", clean(user), clean(&host))))
}

/// Refuses a secret file other people can read.
pub fn check_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        std::fs::metadata(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{} can be read by other users; run `chmod 600 {}` first.",
            path.display(),
            path.display()
        ));
    }
    Ok(())
}

/// The first line of a private file.
pub fn read_secret(path: &Path) -> Result<String, String> {
    check_private(path)?;
    let s = std::fs::read_to_string(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let line = s.lines().next().unwrap_or("").to_string();
    if line.is_empty() {
        return Err(format!("{} is empty.", path.display()));
    }
    Ok(line)
}

/// Writes a token file (mode 0600) into a directory only the owner can enter (mode 0700 when
/// it has to be created).
pub fn save_token(api: &Api, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(d) = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty() && !d.exists())
    {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(d)
            .map_err(|e| e.to_string())?;
    }
    api.save_token(path)
}

/// The token file to use when none is named: the only `*.token` in the config directory.
fn lone_token() -> Option<PathBuf> {
    let mut found = std::fs::read_dir(config_dir()?)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "token"));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// A signed-in API: a valid cached token, else a fresh sign in with the password file (and the
/// token is cached for next time).
pub fn connect(acct: &Account) -> Result<Api, String> {
    let cache = acct
        .token_file
        .clone()
        .or_else(|| match (&acct.server, &acct.user) {
            (Some(s), Some(u)) => token_path(s, u),
            _ => lone_token(),
        });
    let mut why = None;
    if let Some(path) = cache.as_ref().filter(|p| p.exists()) {
        check_private(path)?;
        let api = Api::from_token_file(path)?;
        let same_account = acct
            .server
            .as_ref()
            .is_none_or(|s| crate::api::normalize_url(s) == api.url)
            && acct.user.as_ref().is_none_or(|u| *u == api.username);
        if same_account {
            if api.token_valid()? {
                return Ok(api);
            }
            why = Some(format!(
                "The saved sign-in in {} is no longer valid.",
                path.display()
            ));
        }
    }
    let (Some(server), Some(user), Some(pwf)) = (&acct.server, &acct.user, &acct.password_file)
    else {
        return Err(why.unwrap_or_else(|| {
            "Not signed in: give --server, --user and --password-file (or a token file from `fsheets login`).".into()
        }));
    };
    let pw = read_secret(pwf)?;
    let api = Api::login(server, user, &pw, "fsheets agent")?;
    if let Some(path) = cache.or_else(|| token_path(server, user)) {
        save_token(&api, &path)?;
    }
    Ok(api)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn token_file_names_are_safe() {
        let p = token_path("https://the21th.example.net:8090/", "agent/claude").unwrap();
        let n = p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(n, "agent_claude@the21th.example.net_8090.token");
        assert!(p.parent().unwrap().ends_with("freedomsheets-agents"));
    }

    #[test]
    fn secrets_must_be_private() {
        let dir = std::env::temp_dir().join(format!("fsheets-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("pw");
        std::fs::write(&f, "hunter2\nsecond line\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = read_secret(&f).unwrap_err();
        assert!(e.contains("chmod 600") && !e.contains("hunter2"));
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_secret(&f).unwrap(), "hunter2");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
