//! Metadata in SQLite and document bytes on disk (`files/<item-id>/<version>.xlsx`).
//! Everything here is synchronous; the HTTP layer calls it from blocking tasks.

use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::util::{self, now};

#[derive(Clone, Debug)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub created: i64,
    pub root_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    View = 1,
    Edit = 2,
    Owner = 3,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "view" | "read" => Some(Role::View),
            "edit" | "write" => Some(Role::Edit),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::View => "view",
            Role::Edit => "edit",
            Role::Owner => "owner",
        }
    }

    fn from_db(s: &str) -> Role {
        Role::parse(s).unwrap_or(Role::View)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub kind: Kind,
    pub owner_id: String,
    pub size: i64,
    pub version: i64,
    pub hash: String,
    pub modified: i64,
    pub modified_by: String,
    pub created: i64,
    /// 0, or the time it was moved to the trash.
    pub deleted: i64,
}

impl Item {
    /// `v<version>-<first 12 hex digits of the content hash>`
    pub fn etag(&self) -> String {
        etag(self.version, &self.hash)
    }
}

pub fn etag(version: i64, hash: &str) -> String {
    format!("v{version}-{}", hash.get(..12).unwrap_or(hash))
}

#[derive(Clone, Debug)]
pub struct Version {
    pub version: i64,
    pub size: i64,
    pub hash: String,
    pub modified: i64,
    pub modified_by: String,
}

#[derive(Clone, Debug)]
pub struct Link {
    pub id: String,
    pub item_id: String,
    pub role: Role,
    pub expires: Option<i64>,
    pub pw_hash: Option<String>,
    pub created: i64,
    pub created_by: String,
}

/// Where live co-editing of an item stands.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveState {
    /// The version that contains every operation up to `seq`.
    pub version: i64,
    pub seq: u64,
    /// Sheet ids and order keys of that version ("id:pos,id:pos"), empty when unknown.
    pub sheets: String,
    /// Operations up to here are no longer kept.
    pub floor: u64,
    pub snap_at: i64,
}

/// A committed batch of live operations (JSON).
#[derive(Clone, Debug)]
pub struct LiveOp {
    pub seq: u64,
    pub cid: String,
    pub batch: u64,
    pub actor: String,
    pub ops: String,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct Grant {
    pub user: User,
    pub role: Role,
    /// The folder the permission was granted on, when inherited.
    pub inherited_from: Option<Item>,
}

#[derive(Clone, Debug)]
pub struct DeviceToken {
    pub id: i64,
    pub device: String,
    pub created: i64,
    pub last_used: i64,
}

/// Who is calling: a signed-in user (with the device token row) or a share-link holder.
#[derive(Clone, Debug)]
pub enum Principal {
    User { user: User, token_id: i64 },
    Link { link: Link },
}

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    Conflict(String),
    Invalid(String),
    Forbidden(String),
    Internal(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "Not found."),
            StoreError::Conflict(m) | StoreError::Invalid(m) | StoreError::Forbidden(m) | StoreError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Internal(format!("database: {e}"))
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Internal(format!("storage: {e}"))
    }
}

pub type Res<T> = Result<T, StoreError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnConflict {
    Fail,
    Rename,
    Replace,
}

impl OnConflict {
    pub fn parse(s: &str) -> Option<OnConflict> {
        match s {
            "" | "fail" => Some(OnConflict::Fail),
            "rename" => Some(OnConflict::Rename),
            "replace" => Some(OnConflict::Replace),
            _ => None,
        }
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name TEXT NOT NULL,
    email TEXT NOT NULL DEFAULT '',
    pw_hash TEXT NOT NULL,
    is_admin INTEGER NOT NULL DEFAULT 0,
    disabled INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL,
    root_id TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tokens (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL REFERENCES users(id),
    hash BLOB NOT NULL UNIQUE,
    device TEXT NOT NULL,
    created INTEGER NOT NULL,
    last_used INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    parent_id TEXT REFERENCES items(id),
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
    owner_id TEXT NOT NULL REFERENCES users(id),
    size INTEGER NOT NULL DEFAULT 0,
    version INTEGER NOT NULL DEFAULT 0,
    hash TEXT NOT NULL DEFAULT '',
    modified INTEGER NOT NULL,
    modified_by TEXT NOT NULL,
    created INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    deleted_by TEXT
);
CREATE INDEX IF NOT EXISTS items_parent ON items(parent_id);
CREATE UNIQUE INDEX IF NOT EXISTS items_name ON items(parent_id, name COLLATE NOCASE) WHERE deleted = 0;
CREATE TABLE IF NOT EXISTS versions (
    item_id TEXT NOT NULL REFERENCES items(id),
    version INTEGER NOT NULL,
    size INTEGER NOT NULL,
    hash TEXT NOT NULL,
    modified INTEGER NOT NULL,
    modified_by TEXT NOT NULL,
    PRIMARY KEY (item_id, version)
);
CREATE TABLE IF NOT EXISTS permissions (
    item_id TEXT NOT NULL REFERENCES items(id),
    user_id TEXT NOT NULL REFERENCES users(id),
    role TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    created INTEGER NOT NULL,
    PRIMARY KEY (item_id, user_id)
);
CREATE INDEX IF NOT EXISTS permissions_user ON permissions(user_id);
CREATE TABLE IF NOT EXISTS links (
    id TEXT PRIMARY KEY,
    token_hash BLOB NOT NULL UNIQUE,
    item_id TEXT NOT NULL REFERENCES items(id),
    role TEXT NOT NULL,
    expires INTEGER,
    pw_hash TEXT,
    created INTEGER NOT NULL,
    created_by TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS link_access (
    hash BLOB PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES links(id) ON DELETE CASCADE,
    expires INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS recent (
    user_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_id)
);
CREATE TABLE IF NOT EXISTS live_ops (
    item_id TEXT NOT NULL REFERENCES items(id),
    seq INTEGER NOT NULL,
    cid TEXT NOT NULL,
    batch INTEGER NOT NULL,
    actor TEXT NOT NULL,
    ops TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (item_id, seq)
);
CREATE TABLE IF NOT EXISTS live_state (
    item_id TEXT PRIMARY KEY REFERENCES items(id),
    version INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    sheets TEXT NOT NULL DEFAULT '',
    floor INTEGER NOT NULL DEFAULT 0,
    snap_at INTEGER NOT NULL
);
"#;

const USER_COLS: &str = "id, username, display_name, email, is_admin, disabled, created, root_id";
const ITEM_COLS: &str = "id, parent_id, name, kind, owner_id, size, version, hash, modified, modified_by, created, deleted";

fn user_row(r: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        username: r.get(1)?,
        display_name: r.get(2)?,
        email: r.get(3)?,
        is_admin: r.get::<_, i64>(4)? != 0,
        disabled: r.get::<_, i64>(5)? != 0,
        created: r.get(6)?,
        root_id: r.get(7)?,
    })
}

fn item_row(r: &Row) -> rusqlite::Result<Item> {
    Ok(Item {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        kind: if r.get::<_, String>(3)? == "folder" { Kind::Folder } else { Kind::File },
        owner_id: r.get(4)?,
        size: r.get(5)?,
        version: r.get(6)?,
        hash: r.get(7)?,
        modified: r.get(8)?,
        modified_by: r.get(9)?,
        created: r.get(10)?,
        deleted: r.get(11)?,
    })
}

fn link_row(r: &Row) -> rusqlite::Result<Link> {
    Ok(Link {
        id: r.get(0)?,
        item_id: r.get(1)?,
        role: Role::from_db(&r.get::<_, String>(2)?),
        expires: r.get(3)?,
        pw_hash: r.get(4)?,
        created: r.get(5)?,
        created_by: r.get(6)?,
    })
}

const LINK_COLS: &str = "id, item_id, role, expires, pw_hash, created, created_by";

pub struct Store {
    pub conn: Connection,
    files: PathBuf,
    pub versions_to_keep: u32,
}

impl Store {
    pub fn open(db: &Path, files: &Path, versions_to_keep: u32) -> Res<Store> {
        if let Some(d) = db.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::create_dir_all(files)?;
        let conn = Connection::open(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn, files: files.to_path_buf(), versions_to_keep: versions_to_keep.max(1) })
    }

    pub fn files_dir(&self) -> &Path {
        &self.files
    }

    // ------------------------------------------------------------------
    // Users
    // ------------------------------------------------------------------

    pub fn create_user(&self, username: &str, display_name: &str, email: &str, password: &str, is_admin: bool) -> Res<User> {
        let username = username.trim();
        if !util::valid_username(username) {
            return Err(StoreError::Invalid("A username may contain letters, digits, '.', '-' and '_' (64 characters at most).".into()));
        }
        util::check_new_password(password).map_err(StoreError::Invalid)?;
        if self.user_by_name(username)?.is_some() {
            return Err(StoreError::Conflict(format!("The user '{username}' already exists.")));
        }
        let display = if display_name.trim().is_empty() { username.to_string() } else { display_name.trim().to_string() };
        let (id, root) = (util::new_id(), util::new_id());
        let t = now();
        let hash = util::hash_password(password);
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO users (id, username, display_name, email, pw_hash, is_admin, created, root_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, username, display, email.trim(), hash, is_admin as i64, t, root],
        )?;
        tx.execute(
            "INSERT INTO items (id, parent_id, name, kind, owner_id, modified, modified_by, created) VALUES (?1, NULL, 'My files', 'folder', ?2, ?3, ?2, ?3)",
            params![root, id, t],
        )?;
        tx.commit()?;
        self.user_by_id(&id)?.ok_or(StoreError::NotFound)
    }

    pub fn user_by_name(&self, username: &str) -> Res<Option<User>> {
        Ok(self.conn.query_row(&format!("SELECT {USER_COLS} FROM users WHERE username = ?1"), [username.trim()], user_row).optional()?)
    }

    pub fn user_by_id(&self, id: &str) -> Res<Option<User>> {
        Ok(self.conn.query_row(&format!("SELECT {USER_COLS} FROM users WHERE id = ?1"), [id], user_row).optional()?)
    }

    pub fn users(&self) -> Res<Vec<User>> {
        let mut st = self.conn.prepare(&format!("SELECT {USER_COLS} FROM users ORDER BY username COLLATE NOCASE"))?;
        let v = st.query_map([], user_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Enabled users whose username or display name contains `query` (for the share dialog).
    pub fn search_users(&self, query: &str, limit: usize) -> Res<Vec<User>> {
        let q = query.trim().to_lowercase();
        Ok(self.users()?.into_iter().filter(|u| !u.disabled && (q.is_empty() || u.username.to_lowercase().contains(&q) || u.display_name.to_lowercase().contains(&q))).take(limit).collect())
    }

    /// The password hash of an enabled user (for sign-in).
    pub fn password_hash(&self, user_id: &str) -> Res<String> {
        Ok(self.conn.query_row("SELECT pw_hash FROM users WHERE id = ?1", [user_id], |r| r.get(0))?)
    }

    pub fn set_password(&self, user_id: &str, password: &str) -> Res<()> {
        util::check_new_password(password).map_err(StoreError::Invalid)?;
        self.conn.execute("UPDATE users SET pw_hash = ?1 WHERE id = ?2", params![util::hash_password(password), user_id])?;
        Ok(())
    }

    pub fn set_disabled(&self, user_id: &str, disabled: bool) -> Res<()> {
        self.conn.execute("UPDATE users SET disabled = ?1 WHERE id = ?2", params![disabled as i64, user_id])?;
        if disabled {
            self.revoke_user_tokens(user_id)?;
        }
        Ok(())
    }

    pub fn set_admin(&self, user_id: &str, admin: bool) -> Res<()> {
        self.conn.execute("UPDATE users SET is_admin = ?1 WHERE id = ?2", params![admin as i64, user_id])?;
        Ok(())
    }

    pub fn display_name(&self, user_id: &str) -> String {
        self.user_by_id(user_id).ok().flatten().map(|u| u.display_name).unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // Device tokens and share-link access tokens
    // ------------------------------------------------------------------

    /// A new device token; only its SHA-256 is stored.
    pub fn create_token(&self, user_id: &str, device: &str) -> Res<String> {
        let token = util::new_token();
        let device: String = device.trim().chars().filter(|c| !c.is_control()).take(100).collect();
        let t = now();
        self.conn.execute(
            "INSERT INTO tokens (user_id, hash, device, created, last_used) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![user_id, util::token_hash(&token), if device.is_empty() { "Unknown device".to_string() } else { device }, t],
        )?;
        Ok(token)
    }

    pub fn tokens_of(&self, user_id: &str) -> Res<Vec<DeviceToken>> {
        let mut st = self.conn.prepare("SELECT id, device, created, last_used FROM tokens WHERE user_id = ?1 ORDER BY last_used DESC")?;
        let v = st
            .query_map([user_id], |r| Ok(DeviceToken { id: r.get(0)?, device: r.get(1)?, created: r.get(2)?, last_used: r.get(3)? }))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn revoke_token(&self, token_id: i64) -> Res<()> {
        self.conn.execute("DELETE FROM tokens WHERE id = ?1", [token_id])?;
        Ok(())
    }

    pub fn revoke_user_tokens(&self, user_id: &str) -> Res<usize> {
        Ok(self.conn.execute("DELETE FROM tokens WHERE user_id = ?1", [user_id])?)
    }

    /// Resolves a bearer token: a device token of an enabled user, or a live share-link access token.
    pub fn authenticate(&self, token: &str) -> Res<Option<Principal>> {
        if token.is_empty() || token.len() > 200 {
            return Ok(None);
        }
        let hash = util::token_hash(token);
        let row: Option<(i64, String, Vec<u8>, i64)> =
            self.conn.query_row("SELECT id, user_id, hash, last_used FROM tokens WHERE hash = ?1", [&hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
        if let Some((token_id, user_id, stored, last_used)) = row {
            if !util::ct_eq(&stored, &hash) {
                return Ok(None);
            }
            let Some(user) = self.user_by_id(&user_id)? else { return Ok(None) };
            if user.disabled {
                return Ok(None);
            }
            let t = now();
            if t - last_used >= 60 {
                self.conn.execute("UPDATE tokens SET last_used = ?1 WHERE id = ?2", params![t, token_id])?;
            }
            return Ok(Some(Principal::User { user, token_id }));
        }
        let row: Option<(Vec<u8>, String, i64)> =
            self.conn.query_row("SELECT hash, link_id, expires FROM link_access WHERE hash = ?1", [&hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        if let Some((stored, link_id, expires)) = row {
            if !util::ct_eq(&stored, &hash) || expires <= now() {
                return Ok(None);
            }
            if let Some(link) = self.link(&link_id)? {
                if link.expires.is_none_or(|e| e > now()) {
                    return Ok(Some(Principal::Link { link }));
                }
            }
        }
        Ok(None)
    }

    // ------------------------------------------------------------------
    // Items and access
    // ------------------------------------------------------------------

    pub fn item(&self, id: &str) -> Res<Option<Item>> {
        if !util::valid_id(id) {
            return Ok(None);
        }
        Ok(self.conn.query_row(&format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"), [id], item_row).optional()?)
    }

    /// The item followed by its ancestors up to the root.
    pub fn chain(&self, item: &Item) -> Res<Vec<Item>> {
        let mut out = vec![item.clone()];
        let mut next = item.parent_id.clone();
        while let Some(p) = next {
            if out.len() > 1000 {
                return Err(StoreError::Internal("folder nesting is too deep".into()));
            }
            let Some(it) = self.item(&p)? else { break };
            next = it.parent_id.clone();
            out.push(it);
        }
        Ok(out)
    }

    /// The caller's role on an item; `None` when the caller can't see it. Items in the trash
    /// (or inside a folder in the trash) are invisible unless `include_deleted`.
    pub fn role(&self, who: &Principal, item: &Item, include_deleted: bool) -> Res<Option<Role>> {
        let chain = self.chain(item)?;
        if !include_deleted && chain.iter().any(|i| i.deleted != 0) {
            return Ok(None);
        }
        match who {
            Principal::User { user, .. } => {
                if item.owner_id == user.id {
                    return Ok(Some(Role::Owner));
                }
                let mut best: Option<Role> = None;
                for it in &chain {
                    let r: Option<String> =
                        self.conn.query_row("SELECT role FROM permissions WHERE item_id = ?1 AND user_id = ?2", params![it.id, user.id], |r| r.get(0)).optional()?;
                    if let Some(r) = r {
                        best = best.max(Some(Role::from_db(&r)));
                    }
                }
                Ok(best)
            }
            Principal::Link { link } => Ok(chain.iter().any(|i| i.id == link.item_id).then_some(link.role)),
        }
    }

    pub fn children(&self, parent_id: &str) -> Res<Vec<Item>> {
        let mut st = self.conn.prepare(&format!("SELECT {ITEM_COLS} FROM items WHERE parent_id = ?1 AND deleted = 0"))?;
        let mut v = st.query_map([parent_id], item_row)?.collect::<Result<Vec<_>, _>>()?;
        v.sort_by_key(|i| (i.kind == Kind::File, i.name.to_lowercase()));
        Ok(v)
    }

    pub fn find_child(&self, parent_id: &str, name: &str) -> Res<Option<Item>> {
        let lower = name.to_lowercase();
        Ok(self.children(parent_id)?.into_iter().find(|i| i.name.to_lowercase() == lower))
    }

    /// `name`, or "name (2)", "name (3)"... whichever is free in the folder.
    fn free_name(&self, parent_id: &str, name: &str) -> Res<String> {
        let taken: Vec<String> = self.children(parent_id)?.into_iter().map(|i| i.name.to_lowercase()).collect();
        if !taken.contains(&name.to_lowercase()) {
            return Ok(name.to_string());
        }
        (2..10000).map(|n| util::numbered_name(name, n)).find(|c| !taken.contains(&c.to_lowercase())).ok_or_else(|| StoreError::Conflict("No free name.".into()))
    }

    fn folder(&self, id: &str) -> Res<Item> {
        match self.item(id)? {
            Some(i) if i.kind == Kind::Folder => Ok(i),
            Some(_) => Err(StoreError::Invalid("That isn't a folder.".into())),
            None => Err(StoreError::NotFound),
        }
    }

    pub fn create_folder(&self, parent_id: &str, name: &str, by: &str) -> Res<Item> {
        let name = util::clean_name(name).map_err(StoreError::Invalid)?;
        let parent = self.folder(parent_id)?;
        if self.find_child(&parent.id, &name)?.is_some() {
            return Err(StoreError::Conflict(format!("An item named '{name}' already exists here.")));
        }
        let id = util::new_id();
        let t = now();
        self.conn.execute(
            "INSERT INTO items (id, parent_id, name, kind, owner_id, modified, modified_by, created) VALUES (?1, ?2, ?3, 'folder', ?4, ?5, ?6, ?5)",
            params![id, parent.id, name, parent.owner_id, t, by],
        )?;
        self.item(&id)?.ok_or(StoreError::NotFound)
    }

    /// Creates a file in a folder (the folder owner owns it). `Replace` adds a version to an
    /// existing file of that name.
    pub fn create_file(&self, parent_id: &str, name: &str, data: &[u8], by: &str, on_conflict: OnConflict) -> Res<(Item, bool)> {
        let name = util::clean_name(name).map_err(StoreError::Invalid)?;
        let parent = self.folder(parent_id)?;
        let name = match (self.find_child(&parent.id, &name)?, on_conflict) {
            (None, _) => name,
            (Some(_), OnConflict::Fail) => return Err(StoreError::Conflict(format!("An item named '{name}' already exists here."))),
            (Some(_), OnConflict::Rename) => self.free_name(&parent.id, &name)?,
            (Some(existing), OnConflict::Replace) => {
                if existing.kind == Kind::Folder {
                    return Err(StoreError::Conflict(format!("A folder named '{name}' already exists here.")));
                }
                return Ok((self.write_content(&existing, data, by)?, false));
            }
        };
        let id = util::new_id();
        let t = now();
        let hash = util::sha256_hex(data);
        self.write_blob(&id, 1, data)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO items (id, parent_id, name, kind, owner_id, size, version, hash, modified, modified_by, created) VALUES (?1, ?2, ?3, 'file', ?4, ?5, 1, ?6, ?7, ?8, ?7)",
            params![id, parent.id, name, parent.owner_id, data.len() as i64, hash, t, by],
        )?;
        tx.execute("INSERT INTO versions (item_id, version, size, hash, modified, modified_by) VALUES (?1, 1, ?2, ?3, ?4, ?5)", params![id, data.len() as i64, hash, t, by])?;
        tx.commit()?;
        Ok((self.item(&id)?.ok_or(StoreError::NotFound)?, true))
    }

    /// Saves new content as the next version.
    pub fn write_content(&self, item: &Item, data: &[u8], by: &str) -> Res<Item> {
        if item.kind != Kind::File {
            return Err(StoreError::Invalid("Folders have no content.".into()));
        }
        // Re-read inside the caller's lock so the version always increases by one.
        let cur = self.item(&item.id)?.ok_or(StoreError::NotFound)?;
        let v = cur.version + 1;
        let hash = util::sha256_hex(data);
        let t = now();
        self.write_blob(&cur.id, v, data)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE items SET size = ?1, version = ?2, hash = ?3, modified = ?4, modified_by = ?5 WHERE id = ?6",
            params![data.len() as i64, v, hash, t, by, cur.id],
        )?;
        tx.execute("INSERT INTO versions (item_id, version, size, hash, modified, modified_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![cur.id, v, data.len() as i64, hash, t, by])?;
        tx.commit()?;
        self.prune_versions(&cur.id)?;
        self.item(&cur.id)?.ok_or(StoreError::NotFound)
    }

    fn blob_path(&self, id: &str, version: i64) -> Res<PathBuf> {
        if !util::valid_id(id) || version < 1 {
            return Err(StoreError::Internal("invalid item id".into()));
        }
        Ok(self.files.join(id).join(format!("{version}.xlsx")))
    }

    /// Atomic, durable write: temp file, fsync, rename, fsync the directory.
    fn write_blob(&self, id: &str, version: i64, data: &[u8]) -> Res<()> {
        let path = self.blob_path(id, version)?;
        let dir = path.parent().ok_or_else(|| StoreError::Internal("bad path".into()))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{version}.{}.tmp", util::hex(&util::random_bytes::<6>())));
        let result = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &path)?;
            sync_dir(dir);
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(result?)
    }

    pub fn read_content(&self, item: &Item, version: Option<i64>) -> Res<(Vec<u8>, Version)> {
        let v = version.unwrap_or(item.version);
        let ver = self.version(&item.id, v)?.ok_or(StoreError::NotFound)?;
        let data = std::fs::read(self.blob_path(&item.id, v)?)?;
        Ok((data, ver))
    }

    pub fn version(&self, item_id: &str, v: i64) -> Res<Option<Version>> {
        Ok(self
            .conn
            .query_row("SELECT version, size, hash, modified, modified_by FROM versions WHERE item_id = ?1 AND version = ?2", params![item_id, v], |r| {
                Ok(Version { version: r.get(0)?, size: r.get(1)?, hash: r.get(2)?, modified: r.get(3)?, modified_by: r.get(4)? })
            })
            .optional()?)
    }

    pub fn versions(&self, item_id: &str) -> Res<Vec<Version>> {
        let mut st = self.conn.prepare("SELECT version, size, hash, modified, modified_by FROM versions WHERE item_id = ?1 ORDER BY version DESC")?;
        let v = st
            .query_map([item_id], |r| Ok(Version { version: r.get(0)?, size: r.get(1)?, hash: r.get(2)?, modified: r.get(3)?, modified_by: r.get(4)? }))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Keeps the newest `versions_to_keep` versions plus the first version of each day.
    pub fn prune_versions(&self, item_id: &str) -> Res<()> {
        let all = self.versions(item_id)?;
        let doomed = versions_to_prune(&all, self.versions_to_keep as usize);
        for v in doomed {
            self.conn.execute("DELETE FROM versions WHERE item_id = ?1 AND version = ?2", params![item_id, v])?;
            let _ = std::fs::remove_file(self.blob_path(item_id, v)?);
        }
        Ok(())
    }

    pub fn restore_version(&self, item: &Item, v: i64, by: &str) -> Res<Item> {
        let (data, _) = self.read_content(item, Some(v))?;
        self.write_content(item, &data, by)
    }

    /// Rename and/or move within the same owner's files.
    pub fn update_item(&self, item: &Item, name: Option<&str>, parent: Option<&Item>, by: &str) -> Res<Item> {
        if item.parent_id.is_none() {
            return Err(StoreError::Invalid("Your root folder can't be renamed or moved.".into()));
        }
        let name = match name {
            Some(n) => util::clean_name(n).map_err(StoreError::Invalid)?,
            None => item.name.clone(),
        };
        let parent_id = match parent {
            Some(p) => {
                if p.kind != Kind::Folder {
                    return Err(StoreError::Invalid("Items can only be moved into folders.".into()));
                }
                if p.owner_id != item.owner_id {
                    return Err(StoreError::Invalid("Items can't be moved into another person's files.".into()));
                }
                if self.chain(p)?.iter().any(|a| a.id == item.id) {
                    return Err(StoreError::Invalid("A folder can't be moved into itself.".into()));
                }
                p.id.clone()
            }
            None => item.parent_id.clone().unwrap_or_default(),
        };
        if let Some(other) = self.find_child(&parent_id, &name)? {
            if other.id != item.id {
                return Err(StoreError::Conflict(format!("An item named '{name}' already exists there.")));
            }
        }
        self.conn.execute(
            "UPDATE items SET name = ?1, parent_id = ?2, modified = ?3, modified_by = ?4 WHERE id = ?5",
            params![name, parent_id, now(), by, item.id],
        )?;
        self.item(&item.id)?.ok_or(StoreError::NotFound)
    }

    pub fn trash(&self, item: &Item, by: &str) -> Res<()> {
        if item.parent_id.is_none() {
            return Err(StoreError::Invalid("Your root folder can't be deleted.".into()));
        }
        self.conn.execute("UPDATE items SET deleted = ?1, deleted_by = ?2 WHERE id = ?3", params![now(), by, item.id])?;
        Ok(())
    }

    /// Restores from the trash; to the owner's root when the old folder is gone, renamed when the name is taken.
    pub fn restore(&self, item: &Item) -> Res<Item> {
        if item.deleted == 0 {
            return Ok(item.clone());
        }
        let parent_ok = match &item.parent_id {
            Some(p) => match self.item(p)? {
                Some(pi) => self.chain(&pi)?.iter().all(|i| i.deleted == 0),
                None => false,
            },
            None => true,
        };
        let parent_id = if parent_ok {
            item.parent_id.clone().unwrap_or_default()
        } else {
            self.user_by_id(&item.owner_id)?.map(|u| u.root_id).ok_or(StoreError::NotFound)?
        };
        let name = self.free_name(&parent_id, &item.name)?;
        self.conn.execute("UPDATE items SET deleted = 0, deleted_by = NULL, parent_id = ?1, name = ?2 WHERE id = ?3", params![parent_id, name, item.id])?;
        self.item(&item.id)?.ok_or(StoreError::NotFound)
    }

    /// Items the user owns that are in the trash.
    pub fn trash_of(&self, user_id: &str) -> Res<Vec<Item>> {
        let mut st = self.conn.prepare(&format!("SELECT {ITEM_COLS} FROM items WHERE owner_id = ?1 AND deleted != 0 ORDER BY deleted DESC"))?;
        let v = st.query_map([user_id], item_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    // ------------------------------------------------------------------
    // Sharing
    // ------------------------------------------------------------------

    pub fn grant(&self, item: &Item, user: &User, role: Role, by: &str) -> Res<()> {
        if user.id == item.owner_id {
            return Err(StoreError::Invalid(format!("{} owns this item.", user.display_name)));
        }
        if user.disabled {
            return Err(StoreError::Invalid(format!("The user '{}' is disabled.", user.username)));
        }
        self.conn.execute(
            "INSERT INTO permissions (item_id, user_id, role, granted_by, created) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(item_id, user_id) DO UPDATE SET role = excluded.role, granted_by = excluded.granted_by",
            params![item.id, user.id, role.as_str(), by, now()],
        )?;
        Ok(())
    }

    pub fn revoke(&self, item: &Item, user_id: &str) -> Res<bool> {
        Ok(self.conn.execute("DELETE FROM permissions WHERE item_id = ?1 AND user_id = ?2", params![item.id, user_id])? > 0)
    }

    /// Direct and inherited (from parent folders) permissions on an item.
    pub fn grants(&self, item: &Item) -> Res<Vec<Grant>> {
        let mut out: Vec<Grant> = Vec::new();
        for (i, it) in self.chain(item)?.iter().enumerate() {
            let mut st = self.conn.prepare("SELECT user_id, role FROM permissions WHERE item_id = ?1")?;
            let rows = st.query_map([&it.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            for (uid, role) in rows {
                let Some(user) = self.user_by_id(&uid)? else { continue };
                let g = Grant { user, role: Role::from_db(&role), inherited_from: (i > 0).then(|| it.clone()) };
                match out.iter_mut().find(|x| x.user.id == uid) {
                    Some(x) if g.role > x.role => *x = g,
                    Some(_) => {}
                    None => out.push(g),
                }
            }
        }
        out.sort_by_key(|g| g.user.display_name.to_lowercase());
        Ok(out)
    }

    /// Items shared directly with the user (still visible to them).
    pub fn shared_with(&self, who: &Principal) -> Res<Vec<Item>> {
        let Principal::User { user, .. } = who else { return Ok(Vec::new()) };
        let mut st = self.conn.prepare(&format!(
            "SELECT {} FROM items i JOIN permissions p ON p.item_id = i.id WHERE p.user_id = ?1 AND i.deleted = 0 AND i.owner_id != ?1 ORDER BY i.modified DESC",
            ITEM_COLS.split(", ").map(|c| format!("i.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let items = st.query_map([&user.id], item_row)?.collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for it in items {
            if self.role(who, &it, false)?.is_some() {
                out.push(it);
            }
        }
        Ok(out)
    }

    pub fn touch_recent(&self, user_id: &str, item_id: &str) -> Res<()> {
        self.conn.execute(
            // A sequence (newer than every earlier entry), so opens within one second keep their order.
            "INSERT INTO recent (user_id, item_id, at) VALUES (?1, ?2, MAX(?3, COALESCE((SELECT MAX(at) FROM recent WHERE user_id = ?1), 0) + 1))
             ON CONFLICT(user_id, item_id) DO UPDATE SET at = excluded.at",
            params![user_id, item_id, now()],
        )?;
        self.conn.execute(
            "DELETE FROM recent WHERE user_id = ?1 AND item_id NOT IN (SELECT item_id FROM recent WHERE user_id = ?1 ORDER BY at DESC LIMIT 100)",
            [user_id],
        )?;
        Ok(())
    }

    /// Files the user opened or saved recently and can still see.
    pub fn recent(&self, who: &Principal, limit: usize) -> Res<Vec<Item>> {
        let Principal::User { user, .. } = who else { return Ok(Vec::new()) };
        let mut st = self.conn.prepare("SELECT item_id FROM recent WHERE user_id = ?1 ORDER BY at DESC")?;
        let ids = st.query_map([&user.id], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(it) = self.item(&id)? {
                if it.kind == Kind::File && self.role(who, &it, false)?.is_some() {
                    out.push(it);
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // Share links
    // ------------------------------------------------------------------

    /// Creates a link; returns it with its secret token (shown once).
    pub fn create_link(&self, item: &Item, role: Role, expires: Option<i64>, password: Option<&str>, by: &str) -> Res<(Link, String)> {
        let token = util::new_token();
        let id = util::new_id();
        let pw = match password.filter(|p| !p.is_empty()) {
            Some(p) => {
                if p.len() > 1024 {
                    return Err(StoreError::Invalid("The password is too long.".into()));
                }
                Some(util::hash_password(p))
            }
            None => None,
        };
        self.conn.execute(
            "INSERT INTO links (id, token_hash, item_id, role, expires, pw_hash, created, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, util::token_hash(&token), item.id, role.as_str(), expires, pw, now(), by],
        )?;
        Ok((self.link(&id)?.ok_or(StoreError::NotFound)?, token))
    }

    pub fn link(&self, id: &str) -> Res<Option<Link>> {
        Ok(self.conn.query_row(&format!("SELECT {LINK_COLS} FROM links WHERE id = ?1"), [id], link_row).optional()?)
    }

    pub fn link_by_token(&self, token: &str) -> Res<Option<Link>> {
        if token.is_empty() || token.len() > 200 {
            return Ok(None);
        }
        let hash = util::token_hash(token);
        let row: Option<(Vec<u8>, Link)> = self
            .conn
            .query_row(&format!("SELECT token_hash, {LINK_COLS} FROM links WHERE token_hash = ?1"), [&hash], |r| {
                Ok((r.get(0)?, Link {
                    id: r.get(1)?,
                    item_id: r.get(2)?,
                    role: Role::from_db(&r.get::<_, String>(3)?),
                    expires: r.get(4)?,
                    pw_hash: r.get(5)?,
                    created: r.get(6)?,
                    created_by: r.get(7)?,
                }))
            })
            .optional()?;
        Ok(row.filter(|(h, _)| util::ct_eq(h, &hash)).map(|(_, l)| l))
    }

    pub fn links_of(&self, item_id: &str) -> Res<Vec<Link>> {
        let mut st = self.conn.prepare(&format!("SELECT {LINK_COLS} FROM links WHERE item_id = ?1 ORDER BY created DESC"))?;
        let v = st.query_map([item_id], link_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn delete_link(&self, link_id: &str) -> Res<bool> {
        self.conn.execute("DELETE FROM link_access WHERE link_id = ?1", [link_id])?;
        Ok(self.conn.execute("DELETE FROM links WHERE id = ?1", [link_id])? > 0)
    }

    /// A short-lived bearer token for a link (after its password, if any, was checked).
    pub fn link_access_token(&self, link: &Link, hours: u64) -> Res<(String, i64)> {
        let token = util::new_token();
        let mut expires = now() + (hours.max(1) as i64) * 3600;
        if let Some(e) = link.expires {
            expires = expires.min(e);
        }
        self.conn.execute("DELETE FROM link_access WHERE expires <= ?1", [now()])?;
        self.conn.execute("INSERT INTO link_access (hash, link_id, expires) VALUES (?1, ?2, ?3)", params![util::token_hash(&token), link.id, expires])?;
        Ok((token, expires))
    }

    // ------------------------------------------------------------------
    // Live co-editing: the committed operations and the snapshot they follow
    // ------------------------------------------------------------------

    pub fn live_state(&self, item_id: &str) -> Res<Option<LiveState>> {
        Ok(self
            .conn
            .query_row("SELECT version, seq, sheets, floor, snap_at FROM live_state WHERE item_id = ?1", [item_id], |r| {
                Ok(LiveState { version: r.get(0)?, seq: r.get::<_, i64>(1)? as u64, sheets: r.get(2)?, floor: r.get::<_, i64>(3)? as u64, snap_at: r.get(4)? })
            })
            .optional()?)
    }

    /// The newest committed sequence number of an item.
    pub fn live_head(&self, item_id: &str) -> Res<u64> {
        let ops: Option<i64> = self.conn.query_row("SELECT MAX(seq) FROM live_ops WHERE item_id = ?1", [item_id], |r| r.get(0))?;
        let state = self.live_state(item_id)?.map_or(0, |s| s.seq);
        Ok((ops.unwrap_or(0) as u64).max(state))
    }

    /// What a live session starts from: the current version is at `seq` (its sheet ids in
    /// `sheets`), operations after `floor` are kept. A version saved outside a live session
    /// (or the first live session) starts at the newest sequence number.
    pub fn live_base(&self, item: &Item) -> Res<LiveState> {
        match self.live_state(&item.id)? {
            Some(s) if s.version == item.version => Ok(s),
            _ => {
                let head = self.live_head(&item.id)?;
                let s = LiveState { version: item.version, seq: head, sheets: String::new(), floor: head, snap_at: now() };
                self.conn.execute(
                    "INSERT INTO live_state (item_id, version, seq, sheets, floor, snap_at) VALUES (?1, ?2, ?3, '', ?3, ?4)
                     ON CONFLICT(item_id) DO UPDATE SET version = excluded.version, seq = excluded.seq, sheets = '', floor = excluded.floor, snap_at = excluded.snap_at",
                    params![item.id, item.version, head as i64, s.snap_at],
                )?;
                self.conn.execute("DELETE FROM live_ops WHERE item_id = ?1 AND seq <= ?2", params![item.id, head as i64])?;
                Ok(s)
            }
        }
    }

    pub fn live_ops_after(&self, item_id: &str, seq: u64) -> Res<Vec<LiveOp>> {
        let mut st = self.conn.prepare("SELECT seq, cid, batch, actor, ops, at FROM live_ops WHERE item_id = ?1 AND seq > ?2 ORDER BY seq")?;
        let v = st
            .query_map(params![item_id, seq as i64], |r| {
                Ok(LiveOp { seq: r.get::<_, i64>(0)? as u64, cid: r.get(1)?, batch: r.get::<_, i64>(2)? as u64, actor: r.get(3)?, ops: r.get(4)?, at: r.get(5)? })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn add_live_op(&self, item_id: &str, op: &LiveOp) -> Res<()> {
        self.conn.execute(
            "INSERT INTO live_ops (item_id, seq, cid, batch, actor, ops, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![item_id, op.seq as i64, op.cid, op.batch as i64, op.actor, op.ops, op.at],
        )?;
        Ok(())
    }

    /// Stores a snapshot uploaded by a live client as the next version: it contains every
    /// operation up to `seq`. Operations up to the previous snapshot are pruned (the ones in
    /// between stay for clients reconnecting with an older base).
    pub fn live_snapshot(&self, item: &Item, data: &[u8], by: &str, seq: u64, sheets: &str) -> Res<(Item, LiveState)> {
        let base = self.live_base(item)?;
        let head = self.live_head(&item.id)?;
        if seq < base.seq || seq > head {
            return Err(StoreError::Conflict("That snapshot is out of date.".into()));
        }
        let saved = self.write_content(item, data, by)?;
        let s = LiveState { version: saved.version, seq, sheets: sheets.to_string(), floor: base.seq.max(base.floor), snap_at: now() };
        self.conn.execute(
            "UPDATE live_state SET version = ?1, seq = ?2, sheets = ?3, floor = ?4, snap_at = ?5 WHERE item_id = ?6",
            params![s.version, seq as i64, s.sheets, s.floor as i64, s.snap_at, item.id],
        )?;
        self.conn.execute("DELETE FROM live_ops WHERE item_id = ?1 AND seq <= ?2", params![item.id, s.floor as i64])?;
        Ok((saved, s))
    }

    // ------------------------------------------------------------------
    // Backup
    // ------------------------------------------------------------------

    /// Consistent online copy of the database plus all document versions (unchanged files are skipped).
    pub fn backup(&self, dest: &Path) -> Res<(usize, u64)> {
        std::fs::create_dir_all(dest)?;
        let tmp = dest.join("freedomoffice.db.tmp");
        let _ = std::fs::remove_file(&tmp);
        {
            let mut out = Connection::open(&tmp)?;
            let b = rusqlite::backup::Backup::new(&self.conn, &mut out)?;
            b.run_to_completion(256, std::time::Duration::from_millis(10), None)?;
        }
        std::fs::rename(&tmp, dest.join("freedomoffice.db"))?;
        let (mut n, mut bytes) = (0usize, 0u64);
        let dest_files = dest.join("files");
        for dir in std::fs::read_dir(&self.files)? {
            let dir = dir?;
            let name = dir.file_name().to_string_lossy().to_string();
            if !util::valid_id(&name) || !dir.file_type()?.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(dir.path())? {
                let f = f?;
                let fname = f.file_name().to_string_lossy().to_string();
                if fname.starts_with('.') || !f.file_type()?.is_file() {
                    continue;
                }
                let target = dest_files.join(&name).join(&fname);
                let len = f.metadata()?.len();
                if target.metadata().map(|m| m.len() == len).unwrap_or(false) {
                    continue;
                }
                std::fs::create_dir_all(target.parent().unwrap_or(&dest_files))?;
                std::fs::copy(f.path(), &target)?;
                n += 1;
                bytes += len;
            }
        }
        Ok((n, bytes))
    }
}

/// Version numbers to delete: everything except the newest `keep` and the first of each day.
pub fn versions_to_prune(all: &[Version], keep: usize) -> Vec<i64> {
    let mut sorted: Vec<&Version> = all.iter().collect();
    sorted.sort_by_key(|v| std::cmp::Reverse(v.version));
    let mut first_of_day: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for v in &sorted {
        let d = util::day_of(v.modified);
        let e = first_of_day.entry(d).or_insert(v.version);
        if v.version < *e {
            *e = v.version;
        }
    }
    sorted.iter().skip(keep).filter(|v| first_of_day.get(&util::day_of(v.modified)) != Some(&v.version)).map(|v| v.version).collect()
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(version: i64, modified: i64) -> Version {
        Version { version, size: 0, hash: String::new(), modified, modified_by: String::new() }
    }

    #[test]
    fn pruning_keeps_newest_and_first_of_day() {
        let day = 86400;
        // Day 0: versions 1-3, day 1: versions 4-6.
        let all = vec![v(1, 10), v(2, 20), v(3, 30), v(4, day + 1), v(5, day + 2), v(6, day + 3)];
        let mut doomed = versions_to_prune(&all, 2);
        doomed.sort();
        // Keep 6, 5 (newest), 1 and 4 (first of each day).
        assert_eq!(doomed, vec![2, 3]);
        assert!(versions_to_prune(&all, 10).is_empty());
    }
}
