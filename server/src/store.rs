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
    /// Signed in with a temporary password: must choose a new one before anything else.
    pub must_change_password: bool,
    /// Set on an organisation's own account (it owns the organisation's files and never signs in).
    pub org_id: Option<String>,
}

/// An organisation. Its files belong to its own account (`account_id`, a `users` row with
/// `org_id` set that can't sign in) whose root folder is the organisation's folder.
#[derive(Clone, Debug)]
pub struct Org {
    pub id: String,
    pub name: String,
    pub root_id: String,
    pub account_id: String,
    /// What plain members get on the organisation's folder (edit or view).
    pub member_role: Role,
    pub created: i64,
    pub created_by: String,
}

/// A person's role in an organisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrgRole {
    Member = 1,
    Admin = 2,
    Owner = 3,
}

impl OrgRole {
    pub fn parse(s: &str) -> Option<OrgRole> {
        match s {
            "member" => Some(OrgRole::Member),
            "admin" => Some(OrgRole::Admin),
            "owner" => Some(OrgRole::Owner),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OrgRole::Member => "member",
            OrgRole::Admin => "admin",
            OrgRole::Owner => "owner",
        }
    }

    /// Owners and admins manage the organisation and fully control its files.
    pub fn manages(self) -> bool {
        self >= OrgRole::Admin
    }
}

#[derive(Clone, Debug)]
pub struct Member {
    pub user: User,
    pub role: OrgRole,
    pub created: i64,
}

#[derive(Clone, Debug)]
pub struct Invite {
    pub id: String,
    pub email: String,
    pub org_id: String,
    pub role: OrgRole,
    pub expires: i64,
    pub created: i64,
    pub created_by: String,
    /// 0, or when it was used.
    pub used: i64,
}

/// What accepting an invite with an existing account did.
#[derive(Clone, Debug)]
pub enum InviteAccepted {
    /// Added to the organisation with this role (the invite is used up).
    Joined(User, Org, OrgRole),
    /// Was a member already, with this role; nothing changed and the invite stays usable.
    AlreadyMember(User, Org, OrgRole),
}

/// Bytes of all kept versions of the files an account owns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub files: i64,
    pub versions: i64,
    pub bytes: i64,
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

/// A permission granted to everyone in an organisation.
#[derive(Clone, Debug)]
pub struct OrgGrant {
    pub org: Org,
    pub role: Role,
    pub inherited_from: Option<Item>,
}

#[derive(Clone, Debug)]
pub struct DeviceToken {
    pub id: i64,
    pub device: String,
    pub created: i64,
    pub last_used: i64,
}

/// What a browser session cookie is for: the web admin page (`/admin`, `__Host-fo_admin`) or
/// the JSON API from the browser (Drive, `__Host-fo_web`). Both live in `admin_sessions`; a
/// session of one kind never works as the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    Admin,
    Web,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::Admin => "admin",
            SessionKind::Web => "web",
        }
    }
}

/// What a person signed in with: an API device token, a web admin session or a browser
/// (Drive) session of the API. The ids are `tokens.id` and `admin_sessions.id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignIn {
    Device(i64),
    Admin(i64),
    Web(i64),
}

/// A browser sign-in (`/admin`, or the API from the browser): a cookie, separate from the
/// device tokens.
#[derive(Clone, Debug)]
pub struct AdminSession {
    pub id: i64,
    pub user_id: String,
    pub kind: SessionKind,
    /// The browser's User-Agent (shortened), to tell sessions apart.
    pub agent: String,
    pub created: i64,
    /// Absolute: the session ends then, however active it is.
    pub expires: i64,
    pub last_used: i64,
}

/// How long a web admin session lasts (absolute, from sign-in).
pub const ADMIN_SESSION_SECS: i64 = 12 * 3600;

/// Web admin sessions kept per person; signing in once more ends the oldest.
pub const ADMIN_SESSIONS_PER_USER: i64 = 10;

/// Who is calling: a signed-in user (with how they signed in) or a share-link holder.
#[derive(Clone, Debug)]
pub enum Principal {
    User { user: User, sign_in: SignIn },
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

/// New content for a file: in memory, or an upload already staged (written and synced) on
/// disk under `Store::incoming_dir`, with its size and SHA-256 (hex).
#[derive(Clone, Copy, Debug)]
pub enum Blob<'a> {
    Bytes(&'a [u8]),
    Staged { path: &'a Path, size: i64, hash: &'a str },
}

impl Blob<'_> {
    fn size(&self) -> i64 {
        match self {
            Blob::Bytes(b) => b.len() as i64,
            Blob::Staged { size, .. } => *size,
        }
    }

    fn hash(&self) -> String {
        match self {
            Blob::Bytes(b) => util::sha256_hex(b),
            Blob::Staged { hash, .. } => hash.to_string(),
        }
    }
}

/// An event for the audit log (see `Store::audit`). Never passwords, tokens or file contents.
#[derive(Clone, Debug, Default)]
pub struct Audit {
    pub action: &'static str,
    pub actor_id: String,
    pub actor_name: String,
    /// The client address; empty: the address of the request being served (`Store::request_ip`).
    pub ip: String,
    pub org_id: Option<String>,
    pub item_id: Option<String>,
    pub target_user: String,
    pub detail: String,
}

impl Audit {
    /// Something `user` did (their username is kept as written then).
    pub fn by(user: &User, action: &'static str) -> Audit {
        Audit { action, actor_id: user.id.clone(), actor_name: user.username.clone(), ..Audit::default() }
    }

    /// Something done by a share-link holder or someone not signed in.
    pub fn anonymous(action: &'static str, actor_id: impl Into<String>) -> Audit {
        Audit { action, actor_id: actor_id.into(), ..Audit::default() }
    }

    pub fn org(mut self, org_id: &str) -> Audit {
        self.org_id = Some(org_id.to_string());
        self
    }

    pub fn item(mut self, item_id: &str) -> Audit {
        self.item_id = Some(item_id.to_string());
        self
    }

    pub fn target(mut self, username: &str) -> Audit {
        self.target_user = username.to_string();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Audit {
        self.detail = detail.into();
        self
    }

    pub fn ip(mut self, ip: impl Into<String>) -> Audit {
        self.ip = ip.into();
        self
    }
}

/// A stored audit event.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditEvent {
    pub id: i64,
    pub at: i64,
    pub actor_id: String,
    pub actor_name: String,
    pub ip: String,
    pub action: String,
    pub org_id: Option<String>,
    pub item_id: Option<String>,
    pub target_user: String,
    pub detail: String,
}

/// Which audit events to list: newest first, ids below `before`, at most `limit`.
#[derive(Clone, Debug, Default)]
pub struct AuditQuery {
    pub before: Option<i64>,
    pub limit: usize,
    /// Only events of these organisations (an org admin's view); `None`: every event.
    pub within_orgs: Option<Vec<String>>,
    pub org: Option<String>,
    /// Events done by or to this username.
    pub user: Option<String>,
    /// An action ("org.rename") or a group ("org": every "org.*").
    pub action: Option<String>,
}

/// The original schema (`user_version` 0): created on a new database, then migrated like an
/// existing one, so both take the same path.
const SCHEMA_V0: &str = r#"
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
CREATE TABLE IF NOT EXISTS live_clients (
    item_id TEXT NOT NULL REFERENCES items(id),
    actor TEXT NOT NULL,
    cid TEXT NOT NULL,
    batch INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    PRIMARY KEY (item_id, actor, cid)
);
"#;

/// Version 1: organisations, members, grants to whole organisations, temporary passwords and
/// invites. `permissions` is rebuilt (its key changes); existing grants become user grants.
const MIGRATION_V1: &str = r#"
ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN org_id TEXT;
CREATE TABLE orgs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    root_id TEXT NOT NULL UNIQUE REFERENCES items(id),
    account_id TEXT NOT NULL UNIQUE REFERENCES users(id),
    member_role TEXT NOT NULL DEFAULT 'edit' CHECK (member_role IN ('edit', 'view')),
    created INTEGER NOT NULL,
    created_by TEXT NOT NULL
);
CREATE TABLE org_members (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    added_by TEXT NOT NULL,
    created INTEGER NOT NULL,
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX org_members_user ON org_members(user_id);
CREATE TABLE permissions_v1 (
    item_id TEXT NOT NULL REFERENCES items(id),
    grantee_kind TEXT NOT NULL CHECK (grantee_kind IN ('user', 'org')),
    grantee_id TEXT NOT NULL,
    role TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    created INTEGER NOT NULL,
    PRIMARY KEY (item_id, grantee_kind, grantee_id)
);
INSERT INTO permissions_v1 (item_id, grantee_kind, grantee_id, role, granted_by, created)
    SELECT item_id, 'user', user_id, role, granted_by, created FROM permissions;
DROP TABLE permissions;
ALTER TABLE permissions_v1 RENAME TO permissions;
CREATE INDEX permissions_grantee ON permissions(grantee_kind, grantee_id);
CREATE TABLE invites (
    id TEXT PRIMARY KEY,
    token_hash BLOB NOT NULL UNIQUE,
    email TEXT NOT NULL DEFAULT '',
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    expires INTEGER NOT NULL,
    created INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    used INTEGER NOT NULL DEFAULT 0,
    used_by TEXT
);
"#;

/// Version 2: web admin sessions (`/admin`, separate from the API's device tokens) and the
/// time of each person's last sign-in.
const MIGRATION_V2: &str = r#"
ALTER TABLE users ADD COLUMN last_login INTEGER NOT NULL DEFAULT 0;
CREATE TABLE admin_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL REFERENCES users(id),
    hash BLOB NOT NULL UNIQUE,
    agent TEXT NOT NULL DEFAULT '',
    created INTEGER NOT NULL,
    expires INTEGER NOT NULL,
    last_used INTEGER NOT NULL
);
CREATE INDEX admin_sessions_user ON admin_sessions(user_id);
"#;

/// Version 3 (Drive): browser sessions of the JSON API next to the web admin ones
/// (`admin_sessions.kind`), and the append-only audit log. The triggers refuse every change
/// to an audit row and every deletion of one younger than `AUDIT_KEEP_DAYS` (pruning).
const MIGRATION_V3: &str = r#"
ALTER TABLE admin_sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'admin';
CREATE INDEX admin_sessions_kind ON admin_sessions(user_id, kind);
CREATE TABLE audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    actor_id TEXT NOT NULL DEFAULT '',
    actor_name TEXT NOT NULL DEFAULT '',
    ip TEXT NOT NULL DEFAULT '',
    action TEXT NOT NULL,
    org_id TEXT,
    item_id TEXT,
    target_user TEXT NOT NULL DEFAULT '',
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX audit_at ON audit(at);
CREATE INDEX audit_org ON audit(org_id, id);
CREATE INDEX audit_actor ON audit(actor_name, id);
CREATE INDEX audit_target ON audit(target_user, id);
CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit BEGIN
    SELECT RAISE(ABORT, 'the audit log is append-only');
END;
CREATE TRIGGER audit_no_early_delete BEFORE DELETE ON audit
    WHEN old.at > CAST(strftime('%s', 'now') AS INTEGER) - 365 * 86400 BEGIN
    SELECT RAISE(ABORT, 'audit events are kept for 365 days');
END;
"#;

/// Audit events are kept this long, then pruned (daily).
pub const AUDIT_KEEP_DAYS: i64 = 365;

/// The schema version this code writes (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 3;

/// Brings a database (new, or written by an older server) to `SCHEMA_VERSION`. One write
/// transaction, so a crash leaves the old schema intact, and a second process opening the
/// database at the same time waits and then finds it done.
fn migrate(conn: &Connection) -> Res<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let r = (|| -> Res<()> {
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(StoreError::Internal(format!("the database was written by a newer server (schema {v}, this server knows {SCHEMA_VERSION})")));
        }
        if v < 1 {
            conn.execute_batch(SCHEMA_V0)?;
            conn.execute_batch(MIGRATION_V1)?;
        }
        if v < 2 {
            conn.execute_batch(MIGRATION_V2)?;
        }
        if v < 3 {
            conn.execute_batch(MIGRATION_V3)?;
        }
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        Ok(())
    })();
    match r {
        Ok(()) => Ok(conn.execute_batch("COMMIT")?),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

const USER_COLS: &str = "id, username, display_name, email, is_admin, disabled, created, root_id, must_change_password, org_id";
const ORG_COLS: &str = "id, name, root_id, account_id, member_role, created, created_by";

fn org_row(r: &Row) -> rusqlite::Result<Org> {
    Ok(Org {
        id: r.get(0)?,
        name: r.get(1)?,
        root_id: r.get(2)?,
        account_id: r.get(3)?,
        member_role: Role::from_db(&r.get::<_, String>(4)?),
        created: r.get(5)?,
        created_by: r.get(6)?,
    })
}

fn invite_row(r: &Row) -> rusqlite::Result<Invite> {
    Ok(Invite {
        id: r.get(0)?,
        email: r.get(1)?,
        org_id: r.get(2)?,
        role: OrgRole::parse(&r.get::<_, String>(3)?).unwrap_or(OrgRole::Member),
        expires: r.get(4)?,
        created: r.get(5)?,
        created_by: r.get(6)?,
        used: r.get(7)?,
    })
}

const ADMIN_SESSION_COLS: &str = "id, user_id, agent, created, expires, last_used, kind";

fn admin_session_row(r: &Row, at: usize) -> rusqlite::Result<AdminSession> {
    Ok(AdminSession {
        id: r.get(at)?,
        user_id: r.get(at + 1)?,
        agent: r.get(at + 2)?,
        created: r.get(at + 3)?,
        expires: r.get(at + 4)?,
        last_used: r.get(at + 5)?,
        kind: if r.get::<_, String>(at + 6)? == "web" { SessionKind::Web } else { SessionKind::Admin },
    })
}

const INVITE_COLS: &str ="id, email, org_id, role, expires, created, created_by, used";

/// Invites are valid this long.
pub const INVITE_DAYS: i64 = 7;

/// "?1, ?2, ..." for `n` values starting at `?{from}`.
fn placeholders(from: usize, n: usize) -> String {
    (from..from + n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ")
}
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
        must_change_password: r.get::<_, i64>(8)? != 0,
        org_id: r.get(9)?,
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
    /// `allow_links` from the configuration: when false, share-link access tokens don't
    /// authenticate and links can't be opened.
    pub allow_links: bool,
    /// The client address of the request being served (set by the HTTP layer for each
    /// store call; recorded in audit events).
    pub request_ip: String,
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
        migrate(&conn)?;
        Ok(Store { conn, files: files.to_path_buf(), versions_to_keep: versions_to_keep.max(1), allow_links: true, request_ip: String::new() })
    }

    pub fn files_dir(&self) -> &Path {
        &self.files
    }

    // ------------------------------------------------------------------
    // Users
    // ------------------------------------------------------------------

    pub fn create_user(&self, username: &str, display_name: &str, email: &str, password: &str, is_admin: bool) -> Res<User> {
        self.create_account(username, display_name, email, password, is_admin, false)
    }

    /// A person's account; with `must_change_password` the password is a temporary one.
    /// Checks the password policy and hashes here (for the CLI and tests); the server hashes
    /// outside the store lock and calls `create_account_hashed`.
    pub fn create_account(&self, username: &str, display_name: &str, email: &str, password: &str, is_admin: bool, must_change_password: bool) -> Res<User> {
        util::check_new_password(password, username.trim()).map_err(StoreError::Invalid)?;
        self.create_account_hashed(username, display_name, email, &util::hash_password(password), is_admin, must_change_password)
    }

    /// The username for a new account, trimmed, when it is valid and free.
    pub fn check_new_username(&self, username: &str) -> Res<String> {
        let username = username.trim();
        if !util::valid_username(username) {
            return Err(StoreError::Invalid("A username may contain letters, digits, '.', '-' and '_' (64 characters at most).".into()));
        }
        if self.user_by_name(username)?.is_some() {
            return Err(StoreError::Conflict(format!("The user '{username}' already exists.")));
        }
        Ok(username.to_string())
    }

    /// `create_account` with the password already checked and hashed (`util::hash_password`).
    pub fn create_account_hashed(&self, username: &str, display_name: &str, email: &str, pw_hash: &str, is_admin: bool, must_change_password: bool) -> Res<User> {
        let tx = self.conn.unchecked_transaction()?;
        let id = self.insert_account(username, display_name, email, pw_hash, is_admin, must_change_password)?;
        tx.commit()?;
        self.user_by_id(&id)?.ok_or(StoreError::NotFound)
    }

    /// An account with a temporary password (hash), joined to `org` with a role (added by
    /// the given user id) in the same transaction.
    pub fn create_temp_account(&self, username: &str, display_name: &str, email: &str, pw_hash: &str, org: Option<(&Org, OrgRole, &str)>) -> Res<User> {
        let tx = self.conn.unchecked_transaction()?;
        let id = self.insert_account(username, display_name, email, pw_hash, false, true)?;
        if let Some((o, role, by)) = org {
            self.conn.execute(
                "INSERT INTO org_members (org_id, user_id, role, added_by, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![o.id, id, role.as_str(), by, now()],
            )?;
        }
        tx.commit()?;
        self.user_by_id(&id)?.ok_or(StoreError::NotFound)
    }

    /// The account row and its root folder; the caller holds a transaction.
    fn insert_account(&self, username: &str, display_name: &str, email: &str, pw_hash: &str, is_admin: bool, must_change_password: bool) -> Res<String> {
        let username = self.check_new_username(username)?;
        let display = if display_name.trim().is_empty() { username.clone() } else { display_name.trim().chars().filter(|c| !c.is_control()).take(100).collect() };
        let email: String = email.trim().chars().filter(|c| !c.is_control()).take(254).collect();
        let (id, root) = (util::new_id(), util::new_id());
        let t = now();
        self.conn.execute(
            "INSERT INTO users (id, username, display_name, email, pw_hash, is_admin, created, root_id, must_change_password) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, username, display, email, pw_hash, is_admin as i64, t, root, must_change_password as i64],
        )?;
        self.conn.execute(
            "INSERT INTO items (id, parent_id, name, kind, owner_id, modified, modified_by, created) VALUES (?1, NULL, 'My files', 'folder', ?2, ?3, ?2, ?3)",
            params![root, id, t],
        )?;
        Ok(id)
    }

    /// A person (organisation accounts are never found by name).
    pub fn user_by_name(&self, username: &str) -> Res<Option<User>> {
        Ok(self.conn.query_row(&format!("SELECT {USER_COLS} FROM users WHERE username = ?1 AND org_id IS NULL"), [username.trim()], user_row).optional()?)
    }

    pub fn user_by_id(&self, id: &str) -> Res<Option<User>> {
        Ok(self.conn.query_row(&format!("SELECT {USER_COLS} FROM users WHERE id = ?1"), [id], user_row).optional()?)
    }

    /// Every person (not the organisations' own accounts).
    pub fn users(&self) -> Res<Vec<User>> {
        let mut st = self.conn.prepare(&format!("SELECT {USER_COLS} FROM users WHERE org_id IS NULL ORDER BY username COLLATE NOCASE"))?;
        let v = st.query_map([], user_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Whether two people are in at least one organisation together.
    pub fn shares_org(&self, a: &str, b: &str) -> Res<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM org_members x JOIN org_members y ON x.org_id = y.org_id WHERE x.user_id = ?1 AND y.user_id = ?2 LIMIT 1", params![a, b], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// People `caller` may find in the share dialog: those whose username or display name
    /// contains `query` among the people sharing an organisation with them (the server
    /// administrator: everyone), plus whoever has exactly that username. Enabled accounts only.
    pub fn search_users_for(&self, caller: &User, query: &str, limit: usize) -> Res<Vec<User>> {
        if caller.is_admin {
            return self.search_users(query, limit);
        }
        let q = query.trim().to_lowercase();
        let cols = USER_COLS.split(", ").map(|c| format!("u.{c}")).collect::<Vec<_>>().join(", ");
        let mut st = self.conn.prepare(&format!(
            "SELECT DISTINCT {cols} FROM users u JOIN org_members m ON m.user_id = u.id
             WHERE m.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?1) AND u.disabled = 0 AND u.org_id IS NULL
             ORDER BY u.username COLLATE NOCASE"
        ))?;
        let mut out: Vec<User> = st
            .query_map([&caller.id], user_row)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|u| q.is_empty() || u.username.to_lowercase().contains(&q) || u.display_name.to_lowercase().contains(&q))
            .collect();
        if let Some(exact) = self.user_by_name(&q)?.filter(|u| !u.disabled)
            && !out.iter().any(|u| u.id == exact.id)
        {
            out.insert(0, exact);
        }
        out.truncate(limit);
        Ok(out)
    }

    /// Enabled users whose username or display name contains `query` (everyone; for the server
    /// administrator).
    pub fn search_users(&self, query: &str, limit: usize) -> Res<Vec<User>> {
        let q = query.trim().to_lowercase();
        Ok(self.users()?.into_iter().filter(|u| !u.disabled && (q.is_empty() || u.username.to_lowercase().contains(&q) || u.display_name.to_lowercase().contains(&q))).take(limit).collect())
    }

    /// The password hash of an enabled user (for sign-in).
    pub fn password_hash(&self, user_id: &str) -> Res<String> {
        Ok(self.conn.query_row("SELECT pw_hash FROM users WHERE id = ?1", [user_id], |r| r.get(0))?)
    }

    /// A password the person chose (clears `must_change_password`). Checks and hashes here
    /// (CLI); the server uses `set_password_hash`.
    pub fn set_password(&self, user_id: &str, password: &str) -> Res<()> {
        let u = self.user_by_id(user_id)?.filter(|u| u.org_id.is_none()).ok_or(StoreError::NotFound)?;
        util::check_new_password(password, &u.username).map_err(StoreError::Invalid)?;
        self.set_password_hash(user_id, &util::hash_password(password))
    }

    /// Stores the hash of a password the person chose (checked by the caller); clears
    /// `must_change_password`.
    pub fn set_password_hash(&self, user_id: &str, pw_hash: &str) -> Res<()> {
        self.user_by_id(user_id)?.filter(|u| u.org_id.is_none()).ok_or(StoreError::NotFound)?;
        self.conn.execute("UPDATE users SET pw_hash = ?1, must_change_password = 0 WHERE id = ?2", params![pw_hash, user_id])?;
        Ok(())
    }

    /// Stores the hash of a temporary password (`util::temp_password`), which must be changed at
    /// the next sign-in, and signs the user out everywhere.
    pub fn set_temp_password_hash(&self, user_id: &str, pw_hash: &str) -> Res<usize> {
        self.user_by_id(user_id)?.filter(|u| u.org_id.is_none()).ok_or(StoreError::NotFound)?;
        self.conn.execute("UPDATE users SET pw_hash = ?1, must_change_password = 1 WHERE id = ?2", params![pw_hash, user_id])?;
        self.revoke_user_tokens(user_id)
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
        self.conn.execute("UPDATE users SET last_login = ?1 WHERE id = ?2", params![t, user_id])?;
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

    /// Signs the user out everywhere: every device token and every web admin session.
    pub fn revoke_user_tokens(&self, user_id: &str) -> Res<usize> {
        let n = self.conn.execute("DELETE FROM tokens WHERE user_id = ?1", [user_id])?;
        Ok(n + self.conn.execute("DELETE FROM admin_sessions WHERE user_id = ?1", [user_id])?)
    }

    /// Signs the user out of every device except `keep` (web admin sessions included).
    pub fn revoke_other_tokens(&self, user_id: &str, keep: i64) -> Res<usize> {
        let n = self.conn.execute("DELETE FROM tokens WHERE user_id = ?1 AND id != ?2", params![user_id, keep])?;
        Ok(n + self.conn.execute("DELETE FROM admin_sessions WHERE user_id = ?1", [user_id])?)
    }

    /// Signs the user out of every device and every web admin session except `keep`.
    pub fn revoke_all_but_admin_session(&self, user_id: &str, keep: i64) -> Res<usize> {
        let n = self.conn.execute("DELETE FROM tokens WHERE user_id = ?1", [user_id])?;
        Ok(n + self.conn.execute("DELETE FROM admin_sessions WHERE user_id = ?1 AND id != ?2", params![user_id, keep])?)
    }

    /// Ends one device token of a person (true when it was theirs and existed).
    pub fn revoke_token_of(&self, user_id: &str, token_id: i64) -> Res<bool> {
        Ok(self.conn.execute("DELETE FROM tokens WHERE id = ?1 AND user_id = ?2", params![token_id, user_id])? > 0)
    }

    /// When each person last signed in (Unix seconds; people who never did are missing).
    pub fn last_logins(&self) -> Res<std::collections::HashMap<String, i64>> {
        let mut st = self.conn.prepare("SELECT id, last_login FROM users WHERE org_id IS NULL AND last_login > 0")?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Enabled server administrators.
    pub fn admin_count(&self) -> Res<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1 AND disabled = 0 AND org_id IS NULL", [], |r| r.get(0))?)
    }

    // ------------------------------------------------------------------
    // Web admin sessions
    // ------------------------------------------------------------------

    /// A new web admin session (valid `ADMIN_SESSION_SECS`); returns its secret (only the
    /// SHA-256 is stored). Expired sessions are cleared on the way.
    pub fn create_admin_session(&self, user_id: &str, agent: &str) -> Res<String> {
        self.create_session(user_id, agent, SessionKind::Admin)
    }

    /// A new browser session of `kind` (valid `ADMIN_SESSION_SECS` from now, whatever the
    /// activity); returns its secret (only the SHA-256 is stored). A person keeps at most
    /// `ADMIN_SESSIONS_PER_USER` of each kind (the oldest end). Expired sessions are cleared.
    pub fn create_session(&self, user_id: &str, agent: &str, kind: SessionKind) -> Res<String> {
        let token = util::new_token();
        let agent: String = agent.trim().chars().filter(|c| !c.is_control()).take(120).collect();
        let t = now();
        self.conn.execute("DELETE FROM admin_sessions WHERE expires <= ?1", [t])?;
        self.conn.execute(
            "INSERT INTO admin_sessions (user_id, hash, agent, created, expires, last_used, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6)",
            params![user_id, util::token_hash(&token), agent, t, t + ADMIN_SESSION_SECS, kind.as_str()],
        )?;
        self.conn.execute(
            "DELETE FROM admin_sessions WHERE user_id = ?1 AND kind = ?3 AND id NOT IN (SELECT id FROM admin_sessions WHERE user_id = ?1 AND kind = ?3 ORDER BY id DESC LIMIT ?2)",
            params![user_id, ADMIN_SESSIONS_PER_USER, kind.as_str()],
        )?;
        self.conn.execute("UPDATE users SET last_login = ?1 WHERE id = ?2", params![t, user_id])?;
        Ok(token)
    }

    /// The person behind a web admin session secret: an unexpired session of an enabled
    /// person (never an organisation's account).
    pub fn admin_session(&self, token: &str) -> Res<Option<(User, AdminSession)>> {
        self.session(token, SessionKind::Admin)
    }

    /// The person behind a browser (Drive) session secret of the API.
    pub fn web_session(&self, token: &str) -> Res<Option<(User, AdminSession)>> {
        self.session(token, SessionKind::Web)
    }

    /// The person behind a session secret of `kind`: an unexpired session of an enabled
    /// person (never an organisation's account).
    pub fn session(&self, token: &str, kind: SessionKind) -> Res<Option<(User, AdminSession)>> {
        if token.is_empty() || token.len() > 200 {
            return Ok(None);
        }
        let hash = util::token_hash(token);
        let row: Option<(Vec<u8>, AdminSession)> = self
            .conn
            .query_row(&format!("SELECT hash, {ADMIN_SESSION_COLS} FROM admin_sessions WHERE hash = ?1"), [&hash], |r| Ok((r.get(0)?, admin_session_row(r, 1)?)))
            .optional()?;
        let Some((stored, sess)) = row else { return Ok(None) };
        let t = now();
        if !util::ct_eq(&stored, &hash) || sess.expires <= t || sess.kind != kind {
            return Ok(None);
        }
        let Some(user) = self.user_by_id(&sess.user_id)?.filter(|u| !u.disabled && u.org_id.is_none()) else { return Ok(None) };
        if t - sess.last_used >= 60 {
            self.conn.execute("UPDATE admin_sessions SET last_used = ?1 WHERE id = ?2", params![t, sess.id])?;
        }
        Ok(Some((user, sess)))
    }

    /// Unexpired web admin sessions of a person, most recently used first.
    pub fn admin_sessions_of(&self, user_id: &str) -> Res<Vec<AdminSession>> {
        let mut st = self.conn.prepare(&format!("SELECT {ADMIN_SESSION_COLS} FROM admin_sessions WHERE user_id = ?1 AND expires > ?2 ORDER BY last_used DESC"))?;
        let v = st.query_map(params![user_id, now()], |r| admin_session_row(r, 0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn admin_session_exists(&self, id: i64) -> Res<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM admin_sessions WHERE id = ?1 AND expires > ?2", params![id, now()], |_| Ok(())).optional()?.is_some())
    }

    /// Ends one web admin session (sign-out).
    pub fn end_admin_session(&self, id: i64) -> Res<()> {
        self.conn.execute("DELETE FROM admin_sessions WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Ends the web admin session with this secret, if there is one (whoever's it is: a browser
    /// that signs in again leaves its old session behind).
    pub fn end_admin_session_by_secret(&self, token: &str) -> Res<()> {
        self.end_session_by_secret(token, SessionKind::Admin)
    }

    /// Ends the session of `kind` with this secret, if there is one.
    pub fn end_session_by_secret(&self, token: &str, kind: SessionKind) -> Res<()> {
        if !token.is_empty() && token.len() <= 200 {
            self.conn.execute("DELETE FROM admin_sessions WHERE hash = ?1 AND kind = ?2", params![util::token_hash(token), kind.as_str()])?;
        }
        Ok(())
    }

    /// Ends one web admin session of a person (true when it was theirs and existed).
    pub fn end_admin_session_of(&self, user_id: &str, id: i64) -> Res<bool> {
        Ok(self.conn.execute("DELETE FROM admin_sessions WHERE id = ?1 AND user_id = ?2", params![id, user_id])? > 0)
    }

    /// Whether a device token still exists (it isn't revoked).
    pub fn token_exists(&self, token_id: i64) -> Res<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM tokens WHERE id = ?1", [token_id], |_| Ok(())).optional()?.is_some())
    }

    /// Re-checks who a connection authenticated as earlier: `Err(reason)` when that is no
    /// longer valid (signed out, account disabled, link removed or expired or links switched
    /// off), else the principal with fresh details.
    pub fn recheck_principal(&self, who: &Principal) -> Res<Result<Principal, &'static str>> {
        match who {
            Principal::User { user, sign_in } => {
                let fresh = self.user_by_id(&user.id)?;
                let signed_in = match *sign_in {
                    SignIn::Device(id) => self.token_exists(id)?,
                    SignIn::Admin(id) | SignIn::Web(id) => self.admin_session_exists(id)?,
                };
                Ok(match fresh {
                    Some(u) if u.disabled => Err("Your account was disabled."),
                    None => Err("Your account was removed."),
                    Some(_) if !signed_in => Err("You were signed out."),
                    Some(u) if u.must_change_password => Err("Your password was reset; sign in again."),
                    Some(u) => Ok(Principal::User { user: u, sign_in: *sign_in }),
                })
            }
            Principal::Link { link } => Ok(match self.link(&link.id)? {
                _ if !self.allow_links => Err("Share links were switched off on this server."),
                Some(l) if l.expires.is_none_or(|e| e > now()) => Ok(Principal::Link { link: l }),
                Some(_) => Err("The share link expired."),
                None => Err("The share link was removed."),
            }),
        }
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
            return Ok(Some(Principal::User { user, sign_in: SignIn::Device(token_id) }));
        }
        if !self.allow_links {
            return Ok(None);
        }
        let row: Option<(Vec<u8>, String, i64)> =
            self.conn.query_row("SELECT hash, link_id, expires FROM link_access WHERE hash = ?1", [&hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        if let Some((stored, link_id, expires)) = row {
            if !util::ct_eq(&stored, &hash) || expires <= now() {
                return Ok(None);
            }
            if let Some(link) = self.link(&link_id)?
                && link.expires.is_none_or(|e| e > now())
            {
                return Ok(Some(Principal::Link { link }));
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
    /// One query (a recursive walk up the parents).
    pub fn chain(&self, item: &Item) -> Res<Vec<Item>> {
        let mut st = self.conn.prepare_cached(&format!(
            "WITH RECURSIVE up(id, depth) AS (
                 SELECT parent_id, 1 FROM items WHERE id = ?1 AND parent_id IS NOT NULL
                 UNION ALL
                 SELECT i.parent_id, up.depth + 1 FROM items i JOIN up ON i.id = up.id WHERE i.parent_id IS NOT NULL AND up.depth <= 1001
             )
             SELECT {} FROM up JOIN items it ON it.id = up.id ORDER BY up.depth",
            ITEM_COLS.split(", ").map(|c| format!("it.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let mut out = vec![item.clone()];
        out.extend(st.query_map([&item.id], item_row)?.collect::<Result<Vec<_>, _>>()?);
        if out.len() > 1000 {
            return Err(StoreError::Internal("folder nesting is too deep".into()));
        }
        Ok(out)
    }

    /// The caller's role on an item; `None` when the caller can't see it. Items in the trash
    /// (or inside a folder in the trash) are invisible unless `include_deleted`.
    ///
    /// The highest of (SPEC.md §3): owner of the item; owner or admin of the organisation whose
    /// folder it is in (-> owner); member of that organisation (-> its `member_role`); a grant
    /// to the person, or to an organisation they belong to, on the item or a parent folder.
    /// A share link sees its item and what is inside it. Being a server administrator gives
    /// no access to anyone's documents. Three queries, whatever the depth.
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
                // The organisation whose folder this is in (the chain ends at its root).
                let top = chain.last().map(|i| i.id.as_str()).unwrap_or_default();
                let org: Option<(String, Option<String>)> = self
                    .conn
                    .prepare_cached("SELECT o.member_role, m.role FROM orgs o LEFT JOIN org_members m ON m.org_id = o.id AND m.user_id = ?2 WHERE o.root_id = ?1")?
                    .query_row(params![top, user.id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                if let Some((member_role, Some(mine))) = org {
                    match OrgRole::parse(&mine) {
                        Some(r) if r.manages() => return Ok(Some(Role::Owner)),
                        Some(_) => best = Some(Role::from_db(&member_role).min(Role::Edit)),
                        None => {}
                    }
                }
                let ids: Vec<&str> = chain.iter().map(|i| i.id.as_str()).collect();
                let sql = format!(
                    "SELECT MAX(CASE role WHEN 'edit' THEN 2 ELSE 1 END) FROM permissions WHERE item_id IN ({}) AND (
                         (grantee_kind = 'user' AND grantee_id = ?1)
                         OR (grantee_kind = 'org' AND grantee_id IN (SELECT org_id FROM org_members WHERE user_id = ?1)))",
                    placeholders(2, ids.len())
                );
                let mut values: Vec<&str> = vec![user.id.as_str()];
                values.extend(ids);
                let granted: Option<i64> = self.conn.query_row(&sql, rusqlite::params_from_iter(values), |r| r.get(0))?;
                let granted = match granted {
                    Some(2) => Some(Role::Edit),
                    Some(_) => Some(Role::View),
                    None => None,
                };
                Ok(best.max(granted))
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
        self.create_file_from(parent_id, name, Blob::Bytes(data), by, on_conflict)
    }

    /// `create_file` with the content in memory or staged on disk (`Blob::Staged`, moved into
    /// place; left where it is when this fails).
    pub fn create_file_from(&self, parent_id: &str, name: &str, data: Blob<'_>, by: &str, on_conflict: OnConflict) -> Res<(Item, bool)> {
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
                return Ok((self.write_content_from(&existing, data, by)?, false));
            }
        };
        let id = util::new_id();
        let t = now();
        let (size, hash) = (data.size(), data.hash());
        self.write_blob(&id, 1, data)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO items (id, parent_id, name, kind, owner_id, size, version, hash, modified, modified_by, created) VALUES (?1, ?2, ?3, 'file', ?4, ?5, 1, ?6, ?7, ?8, ?7)",
            params![id, parent.id, name, parent.owner_id, size, hash, t, by],
        )?;
        tx.execute("INSERT INTO versions (item_id, version, size, hash, modified, modified_by) VALUES (?1, 1, ?2, ?3, ?4, ?5)", params![id, size, hash, t, by])?;
        tx.commit()?;
        Ok((self.item(&id)?.ok_or(StoreError::NotFound)?, true))
    }

    /// Saves new content as the next version.
    pub fn write_content(&self, item: &Item, data: &[u8], by: &str) -> Res<Item> {
        self.write_content_from(item, Blob::Bytes(data), by)
    }

    /// `write_content` with the content in memory or staged on disk.
    pub fn write_content_from(&self, item: &Item, data: Blob<'_>, by: &str) -> Res<Item> {
        if item.kind != Kind::File {
            return Err(StoreError::Invalid("Folders have no content.".into()));
        }
        // Re-read inside the caller's lock so the version always increases by one.
        let cur = self.item(&item.id)?.ok_or(StoreError::NotFound)?;
        let v = cur.version + 1;
        let (size, hash) = (data.size(), data.hash());
        let t = now();
        self.write_blob(&cur.id, v, data)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE items SET size = ?1, version = ?2, hash = ?3, modified = ?4, modified_by = ?5 WHERE id = ?6", params![size, v, hash, t, by, cur.id])?;
        tx.execute("INSERT INTO versions (item_id, version, size, hash, modified, modified_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![cur.id, v, size, hash, t, by])?;
        tx.commit()?;
        self.prune_versions(&cur.id)?;
        self.item(&cur.id)?.ok_or(StoreError::NotFound)
    }

    /// Where uploads are staged while they stream in: `incoming/` next to `files/` in the data
    /// directory (the same file system, so the final rename is atomic; never backed up).
    pub fn incoming_dir(&self) -> PathBuf {
        self.files.parent().map(|p| p.join("incoming")).unwrap_or_else(|| self.files.join("..").join("incoming"))
    }

    /// Removes uploads left half-done by a crash.
    pub fn clear_incoming(&self) {
        let _ = std::fs::remove_dir_all(self.incoming_dir());
    }

    fn blob_path(&self, id: &str, version: i64) -> Res<PathBuf> {
        if !util::valid_id(id) || version < 1 {
            return Err(StoreError::Internal("invalid item id".into()));
        }
        Ok(self.files.join(id).join(format!("{version}.xlsx")))
    }

    /// Atomic, durable write: temp file, fsync, rename, fsync the directory. A staged upload
    /// (written and synced by its writer) is renamed into place.
    fn write_blob(&self, id: &str, version: i64, data: Blob<'_>) -> Res<()> {
        let path = self.blob_path(id, version)?;
        match data {
            Blob::Bytes(b) => self.write_file(&path, version, b),
            Blob::Staged { path: staged, .. } => {
                let dir = path.parent().ok_or_else(|| StoreError::Internal("bad path".into()))?;
                std::fs::create_dir_all(dir)?;
                std::fs::rename(staged, &path)?;
                sync_dir(dir);
                Ok(())
            }
        }
    }

    fn write_file(&self, path: &Path, version: i64, data: &[u8]) -> Res<()> {
        let path = path.to_path_buf();
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

    /// The content of a version as an open file (for streaming; version files never change,
    /// and an open file stays readable even if the version is pruned meanwhile) and its size.
    pub fn open_content(&self, item: &Item, version: Option<i64>) -> Res<(std::fs::File, u64, Version)> {
        let v = version.unwrap_or(item.version);
        let ver = self.version(&item.id, v)?.ok_or(StoreError::NotFound)?;
        let f = std::fs::File::open(self.blob_path(&item.id, v)?)?;
        let len = f.metadata()?.len();
        Ok((f, len, ver))
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
            let path = self.blob_path(item_id, v)?;
            let _ = std::fs::remove_file(path.with_extension("live"));
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    /// Saves an older version's content as the newest version (copied on disk, never read
    /// into memory).
    pub fn restore_version(&self, item: &Item, v: i64, by: &str) -> Res<Item> {
        let ver = self.version(&item.id, v)?.ok_or(StoreError::NotFound)?;
        let dir = self.incoming_dir();
        std::fs::create_dir_all(&dir)?;
        let staged = dir.join(format!("{}.restore", util::new_id()));
        let r = (|| -> Res<Item> {
            std::fs::copy(self.blob_path(&item.id, v)?, &staged)?;
            std::fs::File::open(&staged)?.sync_all()?;
            self.write_content_from(item, Blob::Staged { path: &staged, size: ver.size, hash: &ver.hash }, by)
        })();
        let _ = std::fs::remove_file(&staged);
        r
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
        if let Some(other) = self.find_child(&parent_id, &name)?
            && other.id != item.id
        {
            return Err(StoreError::Conflict(format!("An item named '{name}' already exists there.")));
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
        // Files of a deleted organisation stay where nobody can reach them.
        if let Some(owner) = self.user_by_id(&item.owner_id)?
            && let Some(org) = &owner.org_id
            && self.org(org)?.is_none()
        {
            return Err(StoreError::Invalid("The organisation these files belonged to was deleted.".into()));
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

    /// Items in the trash that the user owns or that belong to an organisation the user is in
    /// (callers filter by role).
    pub fn trash_of(&self, user_id: &str) -> Res<Vec<Item>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {ITEM_COLS} FROM items WHERE deleted != 0 AND (owner_id = ?1
                 OR owner_id IN (SELECT o.account_id FROM orgs o JOIN org_members m ON m.org_id = o.id WHERE m.user_id = ?1))
             ORDER BY deleted DESC"
        ))?;
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
        if user.org_id.is_some() {
            return Err(StoreError::Invalid(format!("{} is an organisation; share with the organisation instead.", user.display_name)));
        }
        if user.disabled {
            return Err(StoreError::Invalid(format!("The user '{}' is disabled.", user.username)));
        }
        self.conn.execute(
            "INSERT INTO permissions (item_id, grantee_kind, grantee_id, role, granted_by, created) VALUES (?1, 'user', ?2, ?3, ?4, ?5)
             ON CONFLICT(item_id, grantee_kind, grantee_id) DO UPDATE SET role = excluded.role, granted_by = excluded.granted_by",
            params![item.id, user.id, role.as_str(), by, now()],
        )?;
        Ok(())
    }

    pub fn revoke(&self, item: &Item, user_id: &str) -> Res<bool> {
        Ok(self.conn.execute("DELETE FROM permissions WHERE item_id = ?1 AND grantee_kind = 'user' AND grantee_id = ?2", params![item.id, user_id])? > 0)
    }

    /// Gives everyone in `org` a role on the item (and what is inside it).
    pub fn grant_org(&self, item: &Item, org: &Org, role: Role, by: &str) -> Res<()> {
        if item.owner_id == org.account_id {
            return Err(StoreError::Invalid(format!("This item is in the folder of {}; its members already have access.", org.name)));
        }
        self.conn.execute(
            "INSERT INTO permissions (item_id, grantee_kind, grantee_id, role, granted_by, created) VALUES (?1, 'org', ?2, ?3, ?4, ?5)
             ON CONFLICT(item_id, grantee_kind, grantee_id) DO UPDATE SET role = excluded.role, granted_by = excluded.granted_by",
            params![item.id, org.id, role.as_str(), by, now()],
        )?;
        Ok(())
    }

    pub fn revoke_org(&self, item: &Item, org_id: &str) -> Res<bool> {
        Ok(self.conn.execute("DELETE FROM permissions WHERE item_id = ?1 AND grantee_kind = 'org' AND grantee_id = ?2", params![item.id, org_id])? > 0)
    }

    /// (position in the chain, grantee id, role) of every grant of `kind` on the item and its parents.
    fn chain_grants(&self, chain: &[Item], kind: &str) -> Res<Vec<(usize, String, Role)>> {
        let mut st = self.conn.prepare_cached("SELECT grantee_id, role FROM permissions WHERE item_id = ?1 AND grantee_kind = ?2")?;
        let mut out = Vec::new();
        for (i, it) in chain.iter().enumerate() {
            for row in st.query_map(params![it.id, kind], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (id, role) = row?;
                out.push((i, id, Role::from_db(&role)));
            }
        }
        Ok(out)
    }

    /// Direct and inherited (from parent folders) permissions on an item.
    pub fn grants(&self, item: &Item) -> Res<Vec<Grant>> {
        let chain = self.chain(item)?;
        let mut out: Vec<Grant> = Vec::new();
        for (i, uid, role) in self.chain_grants(&chain, "user")? {
            let Some(user) = self.user_by_id(&uid)? else { continue };
            let g = Grant { user, role, inherited_from: (i > 0).then(|| chain[i].clone()) };
            match out.iter_mut().find(|x| x.user.id == uid) {
                Some(x) if g.role > x.role => *x = g,
                Some(_) => {}
                None => out.push(g),
            }
        }
        out.sort_by_key(|g| g.user.display_name.to_lowercase());
        Ok(out)
    }

    /// Direct and inherited grants to whole organisations.
    pub fn org_grants(&self, item: &Item) -> Res<Vec<OrgGrant>> {
        let chain = self.chain(item)?;
        let mut out: Vec<OrgGrant> = Vec::new();
        for (i, oid, role) in self.chain_grants(&chain, "org")? {
            let Some(org) = self.org(&oid)? else { continue };
            let g = OrgGrant { org, role, inherited_from: (i > 0).then(|| chain[i].clone()) };
            match out.iter_mut().find(|x| x.org.id == oid) {
                Some(x) if g.role > x.role => *x = g,
                Some(_) => {}
                None => out.push(g),
            }
        }
        out.sort_by_key(|g| g.org.name.to_lowercase());
        Ok(out)
    }

    /// Items shared directly with the user or with an organisation they are in (still visible
    /// to them), not counting the organisations' own folders.
    pub fn shared_with(&self, who: &Principal) -> Res<Vec<Item>> {
        let Principal::User { user, .. } = who else { return Ok(Vec::new()) };
        let mut st = self.conn.prepare(&format!(
            "SELECT DISTINCT {} FROM items i JOIN permissions p ON p.item_id = i.id
             WHERE ((p.grantee_kind = 'user' AND p.grantee_id = ?1)
                    OR (p.grantee_kind = 'org' AND p.grantee_id IN (SELECT org_id FROM org_members WHERE user_id = ?1)))
               AND i.deleted = 0 AND i.owner_id != ?1 ORDER BY i.modified DESC",
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
            if let Some(it) = self.item(&id)?
                && it.kind == Kind::File
                && self.role(who, &it, false)?.is_some()
            {
                out.push(it);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // Organisations
    // ------------------------------------------------------------------

    fn clean_org_name(name: &str) -> Res<String> {
        let n = util::clean_name(name).map_err(StoreError::Invalid)?;
        if n.chars().count() > 100 {
            return Err(StoreError::Invalid("An organisation name can be 100 characters at most.".into()));
        }
        Ok(n)
    }

    /// A new organisation with its own account and an empty folder named after it.
    pub fn create_org(&self, name: &str, by: &str) -> Res<Org> {
        let name = Self::clean_org_name(name)?;
        if self.org_by_name(&name)?.is_some() {
            return Err(StoreError::Conflict(format!("The organisation '{name}' already exists.")));
        }
        let (id, account, root) = (util::new_id(), util::new_id(), util::new_id());
        let t = now();
        let tx = self.conn.unchecked_transaction()?;
        // The account can't sign in: no valid password hash, disabled, and never found by name
        // (':' isn't allowed in usernames).
        tx.execute(
            "INSERT INTO users (id, username, display_name, email, pw_hash, is_admin, disabled, created, root_id, org_id) VALUES (?1, ?2, ?3, '', '!', 0, 1, ?4, ?5, ?6)",
            params![account, format!("org:{id}"), name, t, root, id],
        )?;
        tx.execute(
            "INSERT INTO items (id, parent_id, name, kind, owner_id, modified, modified_by, created) VALUES (?1, NULL, ?2, 'folder', ?3, ?4, ?5, ?4)",
            params![root, name, account, t, by],
        )?;
        tx.execute(
            "INSERT INTO orgs (id, name, root_id, account_id, member_role, created, created_by) VALUES (?1, ?2, ?3, ?4, 'edit', ?5, ?6)",
            params![id, name, root, account, t, by],
        )?;
        tx.commit()?;
        self.org(&id)?.ok_or(StoreError::NotFound)
    }

    pub fn org(&self, id: &str) -> Res<Option<Org>> {
        Ok(self.conn.query_row(&format!("SELECT {ORG_COLS} FROM orgs WHERE id = ?1"), [id], org_row).optional()?)
    }

    pub fn org_by_name(&self, name: &str) -> Res<Option<Org>> {
        Ok(self.conn.query_row(&format!("SELECT {ORG_COLS} FROM orgs WHERE name = ?1"), [name.trim()], org_row).optional()?)
    }

    /// By id, else by name.
    pub fn find_org(&self, name_or_id: &str) -> Res<Option<Org>> {
        if util::valid_id(name_or_id)
            && let Some(o) = self.org(name_or_id)?
        {
            return Ok(Some(o));
        }
        self.org_by_name(name_or_id)
    }

    pub fn orgs(&self) -> Res<Vec<Org>> {
        let mut st = self.conn.prepare(&format!("SELECT {ORG_COLS} FROM orgs ORDER BY name COLLATE NOCASE"))?;
        let v = st.query_map([], org_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// The organisations a person belongs to, with their role in each.
    pub fn orgs_of(&self, user_id: &str) -> Res<Vec<(Org, OrgRole)>> {
        let cols = ORG_COLS.split(", ").map(|c| format!("o.{c}")).collect::<Vec<_>>().join(", ");
        let mut st = self.conn.prepare(&format!("SELECT {cols}, m.role FROM orgs o JOIN org_members m ON m.org_id = o.id WHERE m.user_id = ?1 ORDER BY o.name COLLATE NOCASE"))?;
        let v = st
            .query_map([user_id], |r| Ok((org_row(r)?, OrgRole::parse(&r.get::<_, String>(7)?).unwrap_or(OrgRole::Member))))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// The organisation whose folder the item is in.
    pub fn org_of_item(&self, item: &Item) -> Res<Option<Org>> {
        let Some(owner) = self.user_by_id(&item.owner_id)? else { return Ok(None) };
        match owner.org_id {
            Some(id) => self.org(&id),
            None => Ok(None),
        }
    }

    /// Renames the organisation and its folder.
    pub fn rename_org(&self, org: &Org, name: &str) -> Res<Org> {
        let name = Self::clean_org_name(name)?;
        if self.org_by_name(&name)?.is_some_and(|o| o.id != org.id) {
            return Err(StoreError::Conflict(format!("The organisation '{name}' already exists.")));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE orgs SET name = ?1 WHERE id = ?2", params![name, org.id])?;
        tx.execute("UPDATE users SET display_name = ?1 WHERE id = ?2", params![name, org.account_id])?;
        tx.execute("UPDATE items SET name = ?1 WHERE id = ?2", params![name, org.root_id])?;
        tx.commit()?;
        self.org(&org.id)?.ok_or(StoreError::NotFound)
    }

    /// What plain members get on the organisation's folder: view or edit.
    pub fn set_org_member_role(&self, org: &Org, role: Role) -> Res<()> {
        if role == Role::Owner {
            return Err(StoreError::Invalid("member_role must be view or edit.".into()));
        }
        self.conn.execute("UPDATE orgs SET member_role = ?1 WHERE id = ?2", params![role.as_str(), org.id])?;
        Ok(())
    }

    /// Deletes an organisation whose folder is empty (items in its trash are kept, owned by
    /// the organisation's disabled account, but nobody can reach them any more). Members,
    /// grants to the organisation and invites go with it.
    pub fn delete_org(&self, org: &Org) -> Res<()> {
        if !self.children(&org.root_id)?.is_empty() {
            return Err(StoreError::Conflict(format!("The folder of {} isn't empty. Move or delete its files first.", org.name)));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM permissions WHERE grantee_kind = 'org' AND grantee_id = ?1", [&org.id])?;
        tx.execute("DELETE FROM org_members WHERE org_id = ?1", [&org.id])?;
        tx.execute("DELETE FROM invites WHERE org_id = ?1", [&org.id])?;
        tx.execute("DELETE FROM orgs WHERE id = ?1", [&org.id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn members(&self, org_id: &str) -> Res<Vec<Member>> {
        let cols = USER_COLS.split(", ").map(|c| format!("u.{c}")).collect::<Vec<_>>().join(", ");
        let mut st = self.conn.prepare(&format!("SELECT {cols}, m.role, m.created FROM org_members m JOIN users u ON u.id = m.user_id WHERE m.org_id = ?1 ORDER BY u.display_name COLLATE NOCASE"))?;
        let v = st
            .query_map([org_id], |r| Ok(Member { user: user_row(r)?, role: OrgRole::parse(&r.get::<_, String>(10)?).unwrap_or(OrgRole::Member), created: r.get(11)? }))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn member_role(&self, org_id: &str, user_id: &str) -> Res<Option<OrgRole>> {
        let r: Option<String> = self.conn.query_row("SELECT role FROM org_members WHERE org_id = ?1 AND user_id = ?2", params![org_id, user_id], |r| r.get(0)).optional()?;
        Ok(r.and_then(|r| OrgRole::parse(&r)))
    }

    /// Adds a person to the organisation, or changes their role. True when added.
    pub fn set_member(&self, org: &Org, user: &User, role: OrgRole, by: &str) -> Res<bool> {
        if user.org_id.is_some() {
            return Err(StoreError::Invalid("Organisations can't be members.".into()));
        }
        let existed = self.member_role(&org.id, &user.id)?.is_some();
        self.conn.execute(
            "INSERT INTO org_members (org_id, user_id, role, added_by, created) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(org_id, user_id) DO UPDATE SET role = excluded.role",
            params![org.id, user.id, role.as_str(), by, now()],
        )?;
        Ok(!existed)
    }

    pub fn remove_member(&self, org_id: &str, user_id: &str) -> Res<bool> {
        Ok(self.conn.execute("DELETE FROM org_members WHERE org_id = ?1 AND user_id = ?2", params![org_id, user_id])? > 0)
    }

    // ------------------------------------------------------------------
    // Invites
    // ------------------------------------------------------------------

    /// A one-time invite to join `org` (valid `INVITE_DAYS`); returns it with its secret token
    /// (shown once; only its SHA-256 is stored).
    pub fn create_invite(&self, org: &Org, email: &str, role: OrgRole, by: &str) -> Res<(Invite, String)> {
        let email: String = email.trim().chars().filter(|c| !c.is_control()).take(254).collect();
        let token = util::new_token();
        let id = util::new_id();
        let t = now();
        self.conn.execute(
            "INSERT INTO invites (id, token_hash, email, org_id, role, expires, created, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, util::token_hash(&token), email, org.id, role.as_str(), t + INVITE_DAYS * 86400, t, by],
        )?;
        Ok((self.invite(&id)?.ok_or(StoreError::NotFound)?, token))
    }

    pub fn invite(&self, id: &str) -> Res<Option<Invite>> {
        Ok(self.conn.query_row(&format!("SELECT {INVITE_COLS} FROM invites WHERE id = ?1"), [id], invite_row).optional()?)
    }

    /// An unused, unexpired invite.
    pub fn invite_by_token(&self, token: &str) -> Res<Option<Invite>> {
        if token.is_empty() || token.len() > 200 {
            return Ok(None);
        }
        let hash = util::token_hash(token);
        let row: Option<(Vec<u8>, Invite)> = self
            .conn
            .query_row(&format!("SELECT token_hash, {INVITE_COLS} FROM invites WHERE token_hash = ?1"), [&hash], |r| {
                Ok((r.get(0)?, Invite {
                    id: r.get(1)?,
                    email: r.get(2)?,
                    org_id: r.get(3)?,
                    role: OrgRole::parse(&r.get::<_, String>(4)?).unwrap_or(OrgRole::Member),
                    expires: r.get(5)?,
                    created: r.get(6)?,
                    created_by: r.get(7)?,
                    used: r.get(8)?,
                }))
            })
            .optional()?;
        Ok(row.filter(|(h, i)| util::ct_eq(h, &hash) && i.used == 0 && i.expires > now()).map(|(_, i)| i))
    }

    /// Open (unused, unexpired) invites of an organisation.
    pub fn invites_of(&self, org_id: &str) -> Res<Vec<Invite>> {
        let mut st = self.conn.prepare(&format!("SELECT {INVITE_COLS} FROM invites WHERE org_id = ?1 AND used = 0 AND expires > ?2 ORDER BY created DESC"))?;
        let v = st.query_map(params![org_id, now()], invite_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn delete_invite(&self, id: &str) -> Res<bool> {
        Ok(self.conn.execute("DELETE FROM invites WHERE id = ?1", [id])? > 0)
    }

    /// Whether the person who made an invite may still add people with its role: the server
    /// administrator, or an owner/admin of the organisation (an owner for owner invites).
    pub fn invite_still_authorised(&self, inv: &Invite) -> Res<bool> {
        let Some(creator) = self.user_by_id(&inv.created_by)? else { return Ok(false) };
        if creator.disabled || creator.org_id.is_some() {
            return Ok(false);
        }
        if creator.is_admin {
            return Ok(true);
        }
        Ok(match self.member_role(&inv.org_id, &creator.id)? {
            Some(OrgRole::Owner) => true,
            Some(OrgRole::Admin) => inv.role != OrgRole::Owner,
            _ => false,
        })
    }

    /// Uses an invite: creates the account (the password was checked against the policy and
    /// hashed by the caller) and the membership, in one transaction. An invite works once, and
    /// only while whoever made it may still add people; a failed attempt (username taken)
    /// leaves it usable.
    pub fn accept_invite(&self, token: &str, username: &str, display_name: &str, pw_hash: &str) -> Res<(User, Org)> {
        let inv = self.invite_by_token(token)?.ok_or(StoreError::NotFound)?;
        let org = self.org(&inv.org_id)?.ok_or(StoreError::NotFound)?;
        if !self.invite_still_authorised(&inv)? {
            return Err(StoreError::Forbidden("This invite is no longer valid (whoever sent it can't add people any more). Ask for a new one.".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        if self.conn.execute("UPDATE invites SET used = ?1 WHERE id = ?2 AND used = 0", params![now(), inv.id])? != 1 {
            return Err(StoreError::NotFound);
        }
        let id = self.insert_account(username, display_name, &inv.email, pw_hash, false, false)?;
        self.conn.execute("UPDATE invites SET used_by = ?1 WHERE id = ?2", params![id, inv.id])?;
        self.conn.execute(
            "INSERT INTO org_members (org_id, user_id, role, added_by, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![org.id, id, inv.role.as_str(), inv.created_by, now()],
        )?;
        tx.commit()?;
        Ok((self.user_by_id(&id)?.ok_or(StoreError::NotFound)?, org))
    }

    /// Uses an invite for an account that exists already (the caller checked its password):
    /// adds it to the organisation with the invite's role, in one transaction, once. The
    /// account must be enabled and have no temporary password. Someone who is a member
    /// already gets `AlreadyMember` and the invite stays unused (it may have been meant for
    /// someone else who clicked the wrong button, and nothing changed).
    pub fn accept_invite_existing(&self, token: &str, user_id: &str) -> Res<InviteAccepted> {
        let tx = self.conn.unchecked_transaction()?;
        let inv = self.invite_by_token(token)?.ok_or(StoreError::NotFound)?;
        let org = self.org(&inv.org_id)?.ok_or(StoreError::NotFound)?;
        if !self.invite_still_authorised(&inv)? {
            return Err(StoreError::Forbidden("This invite is no longer valid (whoever sent it can't add people any more). Ask for a new one.".into()));
        }
        let user = self.user_by_id(user_id)?.ok_or(StoreError::NotFound)?;
        if user.org_id.is_some() {
            return Err(StoreError::NotFound);
        }
        if user.disabled {
            return Err(StoreError::Forbidden("This account is disabled, so it can't join an organisation. Ask your administrator.".into()));
        }
        if user.must_change_password {
            return Err(StoreError::Forbidden(
                "This account still has a temporary password. Sign in from FreedomOffice Sheets or the web admin and choose your own password first, then open this link again."
                    .into(),
            ));
        }
        if let Some(role) = self.member_role(&org.id, &user.id)? {
            return Ok(InviteAccepted::AlreadyMember(user, org, role));
        }
        if self.conn.execute("UPDATE invites SET used = ?1, used_by = ?2 WHERE id = ?3 AND used = 0", params![now(), user.id, inv.id])? != 1 {
            return Err(StoreError::NotFound);
        }
        self.conn.execute(
            "INSERT INTO org_members (org_id, user_id, role, added_by, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![org.id, user.id, inv.role.as_str(), inv.created_by, now()],
        )?;
        tx.commit()?;
        Ok(InviteAccepted::Joined(user, org, inv.role))
    }

    // ------------------------------------------------------------------
    // Storage use
    // ------------------------------------------------------------------

    /// Per owning account (a person or an organisation's account): files, kept versions and
    /// their bytes, trash included.
    pub fn usage(&self) -> Res<Vec<(String, Usage)>> {
        let mut st = self.conn.prepare(
            "SELECT i.owner_id, COUNT(DISTINCT i.id), COUNT(v.version), COALESCE(SUM(v.size), 0)
             FROM items i JOIN versions v ON v.item_id = i.id WHERE i.kind = 'file' GROUP BY i.owner_id",
        )?;
        let v = st.query_map([], |r| Ok((r.get(0)?, Usage { files: r.get(1)?, versions: r.get(2)?, bytes: r.get(3)? })))?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn usage_of(&self, owner_id: &str) -> Res<Usage> {
        Ok(self.usage()?.into_iter().find(|(o, _)| o == owner_id).map(|(_, u)| u).unwrap_or_default())
    }

    // ------------------------------------------------------------------
    // Share links
    // ------------------------------------------------------------------

    /// Creates a link; returns it with its secret token (shown once).
    /// `pw_hash`: the hash of the link's password, if it has one (`util::hash_password`).
    pub fn create_link(&self, item: &Item, role: Role, expires: Option<i64>, pw_hash: Option<&str>, by: &str) -> Res<(Link, String)> {
        let token = util::new_token();
        let id = util::new_id();
        let pw = pw_hash;
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
    /// (or the first live session) starts at the newest sequence number; when it replaced
    /// content a session followed, one past it, so a client still on the old content (which
    /// reconnects with `since` alone) is below the floor and told to reload.
    pub fn live_base(&self, item: &Item) -> Res<LiveState> {
        match self.live_state(&item.id)? {
            Some(s) if s.version == item.version => Ok(s),
            prev => {
                let head = self.live_head(&item.id)? + u64::from(prev.is_some());
                let s = LiveState { version: item.version, seq: head, sheets: String::new(), floor: head, snap_at: now() };
                self.conn.execute(
                    "INSERT INTO live_state (item_id, version, seq, sheets, floor, snap_at) VALUES (?1, ?2, ?3, '', ?3, ?4)
                     ON CONFLICT(item_id) DO UPDATE SET version = excluded.version, seq = excluded.seq, sheets = '', floor = excluded.floor, snap_at = excluded.snap_at",
                    params![item.id, item.version, head as i64, s.snap_at],
                )?;
                self.conn.execute("DELETE FROM live_ops WHERE item_id = ?1 AND seq <= ?2", params![item.id, head as i64])?;
                self.conn.execute("DELETE FROM live_clients WHERE item_id = ?1", [&item.id])?;
                Ok(s)
            }
        }
    }

    /// The live state stored with a version by `live_snapshot`, if any.
    pub fn live_state_blob(&self, item_id: &str, version: i64) -> Res<Option<Vec<u8>>> {
        let path = self.blob_path(item_id, version)?.with_extension("live");
        match std::fs::read(&path) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO live_ops (item_id, seq, cid, batch, actor, ops, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![item_id, op.seq as i64, op.cid, op.batch as i64, op.actor, op.ops, op.at],
        )?;
        tx.execute(
            "INSERT INTO live_clients (item_id, actor, cid, batch, seq) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(item_id, actor, cid) DO UPDATE SET batch = excluded.batch, seq = excluded.seq",
            params![item_id, op.actor, op.cid, op.batch as i64, op.seq as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Each client's last committed batch: (actor, cid, batch, seq). Kept when the batch is
    /// pruned; forgotten when the live state is reset.
    pub fn live_clients(&self, item_id: &str) -> Res<Vec<(String, String, u64, u64)>> {
        let mut st = self.conn.prepare("SELECT actor, cid, batch, seq FROM live_clients WHERE item_id = ?1")?;
        let v = st
            .query_map([item_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64, r.get::<_, i64>(3)? as u64)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Stores a snapshot uploaded by a live client as the next version: it contains every
    /// operation up to `seq`. Operations up to the previous snapshot are pruned (the ones in
    /// between stay for clients reconnecting with an older base).
    /// `state`: the client's lossless copy of what it synchronises (opaque), kept next to the
    /// version for clients joining later.
    pub fn live_snapshot(&self, item: &Item, data: &[u8], state: &[u8], by: &str, seq: u64, sheets: &str) -> Res<(Item, LiveState)> {
        let base = self.live_base(item)?;
        let head = self.live_head(&item.id)?;
        if seq < base.seq || seq > head {
            return Err(StoreError::Conflict("That snapshot is out of date.".into()));
        }
        if seq == base.seq {
            // The current version already holds everything up to `seq` (several editors
            // leaving at once each upload one): no new version.
            return Ok((item.clone(), base));
        }
        let saved = self.write_content(item, data, by)?;
        if !state.is_empty() {
            self.write_file(&self.blob_path(&item.id, saved.version)?.with_extension("live"), saved.version, state)?;
        }
        let s = LiveState { version: saved.version, seq, sheets: sheets.to_string(), floor: base.seq.max(base.floor), snap_at: now() };
        self.conn.execute(
            "UPDATE live_state SET version = ?1, seq = ?2, sheets = ?3, floor = ?4, snap_at = ?5 WHERE item_id = ?6",
            params![s.version, seq as i64, s.sheets, s.floor as i64, s.snap_at, item.id],
        )?;
        self.conn.execute("DELETE FROM live_ops WHERE item_id = ?1 AND seq <= ?2", params![item.id, s.floor as i64])?;
        Ok((saved, s))
    }

    // ------------------------------------------------------------------
    // Audit log
    // ------------------------------------------------------------------

    /// Appends an event to the audit log (the address is the request's unless `a.ip` is set).
    /// Text is kept to one line and 500 characters.
    pub fn audit(&self, a: Audit) -> Res<()> {
        let clip = |s: &str, n: usize| -> String { s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(n).collect() };
        let ip = if a.ip.is_empty() { self.request_ip.clone() } else { a.ip };
        self.conn.execute(
            "INSERT INTO audit (at, actor_id, actor_name, ip, action, org_id, item_id, target_user, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![now(), clip(&a.actor_id, 80), clip(&a.actor_name, 80), clip(&ip, 64), a.action, a.org_id, a.item_id, clip(&a.target_user, 80), clip(&a.detail, 500)],
        )?;
        Ok(())
    }

    /// `audit` for something done to an item: the event carries the item's organisation (so
    /// its admins see it), if the item is in one.
    pub fn audit_item(&self, a: Audit, item: &Item) -> Res<()> {
        let org = self.org_of_item(item)?.map(|o| o.id);
        self.audit(Audit { org_id: org, ..a }.item(&item.id))
    }

    /// Audit events, newest first (see `AuditQuery`).
    pub fn audit_events(&self, q: &AuditQuery) -> Res<Vec<AuditEvent>> {
        let mut sql = String::from("SELECT id, at, actor_id, actor_name, ip, action, org_id, item_id, target_user, detail FROM audit WHERE 1 = 1");
        use rusqlite::types::Value as V;
        let mut args: Vec<V> = Vec::new();
        // Adds a value and gives its placeholder.
        let mut p = |v: V| {
            args.push(v);
            format!("?{}", args.len())
        };
        if let Some(b) = q.before {
            sql.push_str(&format!(" AND id < {}", p(V::Integer(b))));
        }
        if let Some(orgs) = &q.within_orgs {
            if orgs.is_empty() {
                return Ok(Vec::new());
            }
            let list: Vec<String> = orgs.iter().map(|o| p(V::Text(o.clone()))).collect();
            sql.push_str(&format!(" AND org_id IN ({})", list.join(", ")));
        }
        if let Some(o) = &q.org {
            sql.push_str(&format!(" AND org_id = {}", p(V::Text(o.clone()))));
        }
        if let Some(u) = &q.user {
            let n = p(V::Text(u.clone()));
            sql.push_str(&format!(" AND (actor_name = {n} COLLATE NOCASE OR target_user = {n} COLLATE NOCASE)"));
        }
        if let Some(a) = &q.action {
            // Only letters, '_' and '.' get here (see the API), so no LIKE wildcards.
            let n = p(V::Text(a.clone()));
            sql.push_str(&format!(" AND (action = {n} OR action LIKE {n} || '.%')"));
        }
        sql.push_str(&format!(" ORDER BY id DESC LIMIT {}", p(V::Integer(q.limit.clamp(1, 500) as i64))));
        let mut st = self.conn.prepare(&sql)?;
        let v = st
            .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                Ok(AuditEvent {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    actor_id: r.get(2)?,
                    actor_name: r.get(3)?,
                    ip: r.get(4)?,
                    action: r.get(5)?,
                    org_id: r.get(6)?,
                    item_id: r.get(7)?,
                    target_user: r.get(8)?,
                    detail: r.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Deletes audit events older than `AUDIT_KEEP_DAYS` (the database's own clock, which the
    /// trigger that guards younger events uses too). How many went.
    pub fn prune_audit(&self) -> Res<usize> {
        Ok(self.conn.execute(&format!("DELETE FROM audit WHERE at < CAST(strftime('%s', 'now') AS INTEGER) - {AUDIT_KEEP_DAYS} * 86400"), [])?)
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

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("fo-store-test-{}", util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn user_of(s: &Store, name: &str) -> Principal {
        Principal::User { user: s.user_by_name(name).unwrap().unwrap(), sign_in: SignIn::Device(0) }
    }

    /// A database written by the server before organisations existed (schema 0, with data)
    /// is migrated in place: grants survive as user grants, and opening it again changes nothing.
    #[test]
    fn migrates_a_database_with_the_old_schema() {
        let dir = temp();
        let db = dir.join("freedomoffice.db");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(SCHEMA_V0).unwrap();
            let h = util::hash_password("old-password-123");
            for (id, name, root) in [("a0", "alice", "ra"), ("b0", "bob", "rb")] {
                c.execute("INSERT INTO users (id, username, display_name, pw_hash, created, root_id) VALUES (?1, ?2, ?2, ?3, 1, ?4)", params![id, name, h, root]).unwrap();
                c.execute("INSERT INTO items (id, parent_id, name, kind, owner_id, modified, modified_by, created) VALUES (?1, NULL, 'My files', 'folder', ?2, 1, ?2, 1)", params![root, id]).unwrap();
            }
            let (folder, file) = ("f".repeat(32), "e".repeat(32));
            c.execute("INSERT INTO items (id, parent_id, name, kind, owner_id, modified, modified_by, created) VALUES (?1, 'ra', 'Team', 'folder', 'a0', 1, 'a0', 1)", [&folder]).unwrap();
            c.execute("INSERT INTO items (id, parent_id, name, kind, owner_id, version, modified, modified_by, created) VALUES (?1, ?2, 'Plan.xlsx', 'file', 'a0', 1, 1, 'a0', 1)", params![file, folder]).unwrap();
            c.execute("INSERT INTO versions (item_id, version, size, hash, modified, modified_by) VALUES (?1, 1, 42, 'h', 1, 'a0')", [&file]).unwrap();
            c.execute("INSERT INTO permissions (item_id, user_id, role, granted_by, created) VALUES (?1, 'b0', 'edit', 'a0', 1)", [&folder]).unwrap();
            assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        for _ in 0..2 {
            let s = Store::open(&db, &dir.join("files"), 25).unwrap();
            assert_eq!(s.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
            let file = s.item(&"e".repeat(32)).unwrap().unwrap();
            assert_eq!(s.role(&user_of(&s, "bob"), &file, false).unwrap(), Some(Role::Edit), "the old grant still works");
            assert_eq!(s.role(&user_of(&s, "alice"), &file, false).unwrap(), Some(Role::Owner));
            let g = s.grants(&file).unwrap();
            assert_eq!((g.len(), g[0].user.username.as_str(), g[0].inherited_from.as_ref().map(|i| i.name.as_str())), (1, "bob", Some("Team")));
            let bob = s.user_by_name("bob").unwrap().unwrap();
            assert!(!bob.must_change_password && bob.org_id.is_none());
            assert_eq!(s.usage_of("a0").unwrap(), Usage { files: 1, versions: 1, bytes: 42 });
        }
        // The new tables work on the migrated database.
        let s = Store::open(&db, &dir.join("files"), 25).unwrap();
        let org = s.create_org("Family", "a0").unwrap();
        assert_eq!(s.orgs().unwrap().len(), 1);
        assert!(s.users().unwrap().iter().all(|u| u.org_id.is_none()), "organisation accounts aren't people");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = org;
    }

    /// A database written by the organisations server (schema 1, with organisations, members,
    /// device tokens and grants) gets the web admin tables; everything in it still works and its
    /// foreign keys are intact.
    #[test]
    fn migrates_a_schema_1_database() {
        let dir = temp();
        let (db, files) = (dir.join("freedomoffice.db"), dir.join("files"));
        let (file_id, team_id, token);
        {
            // Exactly what the schema 1 code created, filled through the store functions that
            // existed then (none of them touches the version 2 additions).
            let conn = Connection::open(&db).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(SCHEMA_V0).unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute_batch("PRAGMA user_version = 1").unwrap();
            std::fs::create_dir_all(&files).unwrap();
            let s = Store { conn, files: files.clone(), versions_to_keep: 25, allow_links: true, request_ip: String::new() };
            let alice = s.create_user("alice", "Alice", "", "old-password-123", true).unwrap();
            let bob = s.create_user("bob", "Bob", "", "old-password-123", false).unwrap();
            let carol = s.create_user("carol", "Carol", "", "old-password-123", false).unwrap();
            let team = s.create_org("Team", &alice.id).unwrap();
            s.set_member(&team, &bob, OrgRole::Admin, &alice.id).unwrap();
            s.set_member(&team, &carol, OrgRole::Member, &alice.id).unwrap();
            let (org_file, _) = s.create_file(&team.root_id, "Org.xlsx", b"org", &bob.id, OnConflict::Fail).unwrap();
            let (mine, _) = s.create_file(&alice.root_id, "Mine.xlsx", b"mine", &alice.id, OnConflict::Fail).unwrap();
            s.grant(&mine, &carol, Role::View, &alice.id).unwrap();
            s.grant_org(&mine, &team, Role::Edit, &alice.id).unwrap();
            let t = util::new_token();
            s.conn.execute("INSERT INTO tokens (user_id, hash, device, created, last_used) VALUES (?1, ?2, 'laptop', 1, 1)", params![bob.id, util::token_hash(&t)]).unwrap();
            assert!(s.conn.prepare("SELECT 1 FROM admin_sessions").is_err(), "a real schema 1 database");
            (file_id, team_id, token) = (mine.id, team.id, t);
            let _ = org_file;
        }
        let s = Store::open(&db, &files, 25).unwrap();
        assert_eq!(s.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
        let fk: Vec<String> = s.conn.prepare("PRAGMA foreign_key_check").unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert!(fk.is_empty(), "foreign key problems in {fk:?}");
        // Organisations, members, tokens and grants survive.
        let team = s.org(&team_id).unwrap().unwrap();
        let roles: Vec<(String, OrgRole)> = s.members(&team.id).unwrap().into_iter().map(|m| (m.user.username, m.role)).collect();
        assert_eq!(roles, vec![("bob".to_string(), OrgRole::Admin), ("carol".to_string(), OrgRole::Member)]);
        let Some(Principal::User { user, .. }) = s.authenticate(&token).unwrap() else { panic!("the device token still works") };
        assert_eq!(user.username, "bob");
        let mine = s.item(&file_id).unwrap().unwrap();
        assert_eq!(s.role(&user_of(&s, "carol"), &mine, false).unwrap(), Some(Role::Edit), "max of her view grant and Team's edit grant");
        assert_eq!(s.role(&user_of(&s, "bob"), &mine, false).unwrap(), Some(Role::Edit));
        assert_eq!(s.org_grants(&mine).unwrap().len(), 1);
        let org_file = s.find_child(&team.root_id, "Org.xlsx").unwrap().unwrap();
        assert_eq!(s.role(&user_of(&s, "bob"), &org_file, false).unwrap(), Some(Role::Owner));
        assert_eq!(s.usage_of(&team.account_id).unwrap().bytes, 3);
        // The version 2 additions work.
        assert!(s.last_logins().unwrap().is_empty(), "nobody signed in since the upgrade");
        let secret = s.create_admin_session(&user.id, "browser").unwrap();
        assert_eq!(s.admin_session(&secret).unwrap().unwrap().0.username, "bob");
        assert!(s.last_logins().unwrap().contains_key(&user.id));
        drop(s);
        // Opening again changes nothing.
        let s = Store::open(&db, &files, 25).unwrap();
        assert!(s.admin_session(&secret).unwrap().is_some());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Web admin sessions: absolute expiry, at most ADMIN_SESSIONS_PER_USER, ended by secret.
    #[test]
    fn admin_sessions_expire_and_are_capped() {
        let dir = temp();
        let s = Store::open(&dir.join("db"), &dir.join("files"), 25).unwrap();
        let u = s.create_user("ann", "Ann", "", "old-password-123", false).unwrap();
        let first = s.create_admin_session(&u.id, "").unwrap();
        s.conn.execute("UPDATE admin_sessions SET expires = ?1", [now() - 1]).unwrap();
        assert!(s.admin_session(&first).unwrap().is_none(), "expired");
        assert!(s.admin_sessions_of(&u.id).unwrap().is_empty());
        let secrets: Vec<String> = (0..ADMIN_SESSIONS_PER_USER + 2).map(|_| s.create_admin_session(&u.id, "").unwrap()).collect();
        assert_eq!(s.admin_sessions_of(&u.id).unwrap().len() as i64, ADMIN_SESSIONS_PER_USER);
        assert!(s.admin_session(&secrets[0]).unwrap().is_none() && s.admin_session(&secrets[1]).unwrap().is_none(), "the oldest went");
        assert!(s.admin_session(secrets.last().unwrap()).unwrap().is_some());
        s.end_admin_session_by_secret(secrets.last().unwrap()).unwrap();
        assert!(s.admin_session(secrets.last().unwrap()).unwrap().is_none());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SPEC §3: org admins own the org's files, members get the member role, grants to a whole
    /// organisation reach its members, and the server administrator sees nothing implicitly.
    #[test]
    fn roles_from_organisations() {
        let dir = temp();
        let s = Store::open(&dir.join("db"), &dir.join("files"), 25).unwrap();
        let pw = "test-password-1";
        let admin = s.create_user("root", "", "", pw, true).unwrap();
        let (ann, ben, cat) = (s.create_user("ann", "", "", pw, false).unwrap(), s.create_user("ben", "", "", pw, false).unwrap(), s.create_user("cat", "", "", pw, false).unwrap());
        let org = s.create_org("Team", &admin.id).unwrap();
        assert!(s.create_org("team", &admin.id).is_err(), "names are unique, ignoring case");
        let folder = s.create_folder(&org.root_id, "Docs", &ann.id).unwrap();
        let (file, _) = s.create_file(&folder.id, "Plan.xlsx", b"x", &ann.id, OnConflict::Fail).unwrap();
        assert_eq!(file.owner_id, org.account_id, "files in the org folder belong to the org");
        let p = |u: &User| Principal::User { user: u.clone(), sign_in: SignIn::Device(0) };
        assert_eq!(s.role(&p(&admin), &file, false).unwrap(), None, "no implicit access for the server admin");
        s.set_member(&org, &ann, OrgRole::Admin, &admin.id).unwrap();
        s.set_member(&org, &ben, OrgRole::Member, &admin.id).unwrap();
        assert_eq!(s.role(&p(&ann), &file, false).unwrap(), Some(Role::Owner));
        assert_eq!(s.role(&p(&ben), &file, false).unwrap(), Some(Role::Edit));
        s.set_org_member_role(&org, Role::View).unwrap();
        assert_eq!(s.role(&p(&ben), &file, false).unwrap(), Some(Role::View));
        // A personal grant raises it again.
        s.grant(&folder, &ben, Role::Edit, &ann.id).unwrap();
        assert_eq!(s.role(&p(&ben), &file, false).unwrap(), Some(Role::Edit));
        // Grants to an organisation reach its members.
        let (mine, _) = s.create_file(&cat.root_id, "Cat.xlsx", b"c", &cat.id, OnConflict::Fail).unwrap();
        assert_eq!(s.role(&p(&ben), &mine, false).unwrap(), None);
        s.grant_org(&mine, &org, Role::View, &cat.id).unwrap();
        assert_eq!(s.role(&p(&ben), &mine, false).unwrap(), Some(Role::View));
        assert_eq!(s.shared_with(&p(&ben)).unwrap().len(), 2);
        assert_eq!(s.org_grants(&mine).unwrap()[0].org.name, "Team");
        s.remove_member(&org.id, &ben.id).unwrap();
        assert_eq!(s.role(&p(&ben), &mine, false).unwrap(), None);
        assert_eq!(s.role(&p(&ben), &file, false).unwrap(), Some(Role::Edit), "the personal grant stays");
        assert!(s.grant(&file, &s.user_by_id(&org.account_id).unwrap().unwrap(), Role::Edit, &ann.id).is_err());
        // Deleting needs an empty folder.
        assert!(s.delete_org(&org).is_err());
        s.trash(&folder, &ann.id).unwrap();
        assert_eq!(s.trash_of(&ann.id).unwrap().len(), 1, "org admins see the org's trash");
        s.delete_org(&org).unwrap();
        assert!(s.org(&org.id).unwrap().is_none());
        let trashed = s.item(&folder.id).unwrap().unwrap();
        assert!(s.restore(&trashed).is_err(), "nothing comes back into a deleted organisation");
        let account = s.user_by_id(&org.account_id).unwrap().unwrap();
        assert!(s.grant(&mine, &account, Role::View, &cat.id).unwrap_err().to_string().contains("organisation"));
        assert_eq!(s.role(&p(&ann), &file, true).unwrap(), None);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invites_work_once() {
        let dir = temp();
        let s = Store::open(&dir.join("db"), &dir.join("files"), 25).unwrap();
        let org = s.create_org("Club", "x").unwrap();
        let boss = s.create_user("boss", "", "", "test-password-1", false).unwrap();
        s.set_member(&org, &boss, OrgRole::Admin, "x").unwrap();
        let (inv, token) = s.create_invite(&org, "new@example.test", OrgRole::Member, &boss.id).unwrap();
        // An admin's owner invite, or any invite after its maker lost the right to add people, is void.
        let (_, owner_token) = s.create_invite(&org, "", OrgRole::Owner, &boss.id).unwrap();
        let h0 = util::hash_password("test-password-1");
        assert!(matches!(s.accept_invite(&owner_token, "sneaky", "", &h0), Err(StoreError::Forbidden(_))));
        assert_eq!(s.invite_by_token(&token).unwrap().unwrap().id, inv.id);
        let h = util::hash_password("test-password-1");
        s.create_user("taken", "", "", "test-password-1", false).unwrap();
        assert!(s.accept_invite(&token, "taken", "", &h).is_err(), "username taken");
        assert!(s.invite_by_token(&token).unwrap().is_some(), "still usable after a failed attempt");
        assert!(s.user_by_name("taken").unwrap().is_some());
        let (u, o) = s.accept_invite(&token, "newbie", "New Bie", &h).unwrap();
        assert_eq!((u.email.as_str(), o.id.as_str()), ("new@example.test", org.id.as_str()));
        assert_eq!(s.member_role(&org.id, &u.id).unwrap(), Some(OrgRole::Member));
        assert!(s.invite_by_token(&token).unwrap().is_none());
        assert!(s.accept_invite(&token, "other", "", &h).is_err());
        assert!(s.user_by_name("other").unwrap().is_none(), "nothing half-created");
        let (_, t2) = s.create_invite(&org, "", OrgRole::Member, &boss.id).unwrap();
        s.set_member(&org, &boss, OrgRole::Member, "x").unwrap();
        assert!(matches!(s.accept_invite(&t2, "late", "", &h), Err(StoreError::Forbidden(_))));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invites_can_be_accepted_by_existing_accounts() {
        let dir = temp();
        let s = Store::open(&dir.join("db"), &dir.join("files"), 25).unwrap();
        let org = s.create_org("Club", "x").unwrap();
        let boss = s.create_user("boss", "", "", "test-password-1", false).unwrap();
        s.set_member(&org, &boss, OrgRole::Owner, "x").unwrap();
        let old = s.create_user("oldie", "", "", "test-password-1", false).unwrap();
        let (_, token) = s.create_invite(&org, "someone@example.test", OrgRole::Admin, &boss.id).unwrap();
        // A disabled account and one with a temporary password are refused; the invite stays.
        s.set_disabled(&old.id, true).unwrap();
        assert!(matches!(s.accept_invite_existing(&token, &old.id), Err(StoreError::Forbidden(_))));
        s.set_disabled(&old.id, false).unwrap();
        s.conn.execute("UPDATE users SET must_change_password = 1 WHERE id = ?1", [&old.id]).unwrap();
        assert!(matches!(s.accept_invite_existing(&token, &old.id), Err(StoreError::Forbidden(_))));
        s.conn.execute("UPDATE users SET must_change_password = 0 WHERE id = ?1", [&old.id]).unwrap();
        assert!(s.member_role(&org.id, &old.id).unwrap().is_none());
        // A member already: nothing changes and the invite isn't used.
        assert!(matches!(s.accept_invite_existing(&token, &boss.id), Ok(InviteAccepted::AlreadyMember(_, _, OrgRole::Owner))));
        assert!(s.invite_by_token(&token).unwrap().is_some());
        // Joined with the invite's role (the email doesn't have to match); single use.
        assert!(matches!(s.accept_invite_existing(&token, &old.id), Ok(InviteAccepted::Joined(_, _, OrgRole::Admin))));
        assert_eq!(s.member_role(&org.id, &old.id).unwrap(), Some(OrgRole::Admin));
        assert_eq!(s.user_by_id(&old.id).unwrap().unwrap().email, "", "the account keeps its own email");
        assert!(s.invite_by_token(&token).unwrap().is_none());
        assert!(matches!(s.accept_invite_existing(&token, &old.id), Err(StoreError::NotFound)));
        // Void once its maker may no longer add people.
        let other = s.create_user("other", "", "", "test-password-1", false).unwrap();
        let (_, t2) = s.create_invite(&org, "", OrgRole::Member, &old.id).unwrap();
        s.set_member(&org, &old, OrgRole::Member, "x").unwrap();
        assert!(matches!(s.accept_invite_existing(&t2, &other.id), Err(StoreError::Forbidden(_))));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
