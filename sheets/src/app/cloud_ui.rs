//! Cloud documents in the UI: Account page, OneDrive / SharePoint and FreedomOffice Server
//! places in Open and Save As, saving back with conflict handling, AutoSave, the Share button
//! and dialog, and version history for server documents.
//! Network work runs on background threads; results come back over a channel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Align, Align2, Color32, Pos2, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::chrome::Backstage;
use super::dialogs::Dialog;
use super::theme::{self, ic};
use super::{App, Pending};
use crate::cloud::server::{self, Me, OrgInfo, VersionInfo};
use crate::cloud::{self, graph, CloudDoc, Conn, DriveItem, Kind, Permission, Provider, ServerSession, Session, Target, UploadError, User};

const SHARE: char = '\u{E72D}';
const CLOUD: char = '\u{E753}';
const PEOPLE: char = '\u{E716}';
const GLOBE: char = '\u{E774}';
const CLOCK: char = '\u{E823}';
const PC: char = '\u{E7F8}';
const SERVER: char = '\u{E968}';
const HISTORY: char = '\u{E81C}';

const ENTRA_URL: &str = "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade";

/// The steps shown on the Account page when no app registration is configured.
const SETUP_STEPS: &[&str] = &[
    "Sign in to the Microsoft Entra admin center (entra.microsoft.com), open App registrations and select New registration.",
    "Name it, for example \"FreedomOffice Sheets\". Under Supported account types choose \"Accounts in any organizational directory and personal Microsoft accounts\". Under Redirect URI choose \"Public client/native (mobile & desktop)\" and enter http://localhost. Select Register.",
    "Open Authentication, set \"Allow public client flows\" to Yes and select Save.",
    "Open API permissions > Add a permission > Microsoft Graph > Delegated permissions and add openid, profile, offline_access, User.Read, Files.ReadWrite.All and Sites.Read.All. If your organization requires it, ask an admin to grant consent.",
    "Copy the Application (client) ID from the Overview page and paste it below.",
];

/// Link expiry choices in the server Share dialog (days; 0 = never).
const LINK_DAYS: &[(u32, &str)] = &[(0, "Never expires"), (1, "Expires in 1 day"), (7, "Expires in 7 days"), (30, "Expires in 30 days"), (90, "Expires in 90 days")];

/// The views of a FreedomOffice server place: My files, Shared with me, each organisation the
/// person belongs to, Recent.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ServerView {
    #[default]
    MyFiles,
    Shared,
    Recent,
    /// An organisation's folder (index into the account's `Me::orgs`).
    Org(usize),
}

/// A place in File > Open / Save As.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Place {
    /// Recent files / This PC (the existing local page).
    #[default]
    Local,
    MyFiles,
    Recent,
    Shared,
    Sites,
    /// "FreedomOffice Server" before any server is added.
    AddServer,
    /// A signed-in server (index into `CloudState::servers`).
    Server(usize, ServerView),
}

impl Place {
    fn target(self) -> Option<Target> {
        match self {
            Place::Local | Place::AddServer | Place::Server(_, ServerView::Org(_)) => None,
            Place::MyFiles | Place::Server(_, ServerView::MyFiles) => Some(Target::MyFiles),
            Place::Recent | Place::Server(_, ServerView::Recent) => Some(Target::Recent),
            Place::Shared | Place::Server(_, ServerView::Shared) => Some(Target::Shared),
            Place::Sites => Some(Target::Sites),
        }
    }

    /// Same row in the places column (the server view doesn't matter).
    fn same_row(self, other: Place) -> bool {
        match (self, other) {
            (Place::Server(a, _), Place::Server(b, _)) => a == b,
            _ => self == other,
        }
    }
}

#[derive(Default)]
pub struct Browser {
    pub root: Option<Place>,
    /// Breadcrumb: (label, target); the last one is listed.
    pub crumbs: Vec<(String, Target)>,
    pub items: Vec<DriveItem>,
    pub loading: bool,
    pub error: Option<String>,
    /// Name typed for a new folder (Some while the box is shown).
    pub new_folder: Option<String>,
    seq: u64,
}

enum UploadKind {
    /// Save over the open document (with its eTag unless overwriting).
    Save,
    /// "Save a Copy" next to the open document after a conflict.
    Copy,
    /// Save As into a cloud folder.
    New { folder: String, name: String },
}

enum Event {
    Session { result: Result<Option<(Arc<Session>, User)>, String>, interactive: bool },
    Listing(u64, Result<Vec<DriveItem>, String>),
    Opened(Result<CloudDoc, String>),
    /// `opened`: a new server file downloaded again as Open does (where its live session starts).
    Uploaded { kind: UploadKind, local: PathBuf, conn: Conn, result: Result<DriveItem, UploadError>, opened: Option<Result<CloudDoc, String>> },
    Invited(Result<usize, String>),
    Link(Result<String, String>),
    Permissions(Result<Vec<Permission>, String>),
    ServerSignedIn(Result<(Arc<ServerSession>, Option<Me>), String>),
    ServerChecked { url: String, username: String, result: Result<Me, String> },
    PasswordChanged { url: String, result: Result<u64, String> },
    OrgShared(Result<String, String>),
    Users(u64, Result<Vec<(String, String)>, String>),
    FolderCreated(Result<DriveItem, String>),
    Versions(Result<Vec<VersionInfo>, String>),
    VersionRestored(Result<DriveItem, String>),
    VersionCopy(Result<PathBuf, String>),
}

pub enum Conflict {
    /// HTTP 412 on save.
    Changed,
    /// Save As target name exists.
    Exists { conn: Conn, folder: String, name: String, local: PathBuf },
}

#[derive(Default)]
pub struct ShareState {
    pub tab: u8,
    pub emails: String,
    pub can_edit: bool,
    pub message: String,
    pub notify: bool,
    pub link_anyone: bool,
    pub link_edit: bool,
    pub link: Option<String>,
    pub perms: Option<Vec<Permission>>,
    pub busy: bool,
    pub info: Option<String>,
    pub error: Option<String>,
    // FreedomOffice server: people search, chosen people, link options.
    pub user_query: String,
    searched: String,
    user_seq: u64,
    pub user_results: Vec<(String, String)>,
    pub invite: Vec<(String, String)>,
    pub link_days: u32,
    pub link_password: String,
    /// The people search got the keyboard focus once (when the dialog opened).
    focused: bool,
    /// Sharing with a whole organisation: which one (index into the account's organisations)
    /// and whether everyone in it can edit.
    pub org_pick: usize,
    pub org_edit: bool,
}

/// A FreedomOffice server account in this session.
pub struct ServerEntry {
    pub session: Arc<ServerSession>,
    /// Set when the server rejected the device token (signed out elsewhere, revoked).
    pub error: Option<String>,
    /// Who is signed in, with their organisations (once `/api/me` answered).
    pub me: Option<Me>,
}

impl ServerEntry {
    fn orgs(&self) -> &[OrgInfo] {
        self.me.as_ref().map_or(&[], |m| m.orgs.as_slice())
    }
}

/// "Choose a new password" (asked by the server after a temporary password) and
/// File > Account > Change password. The passwords are sent once and then dropped.
#[derive(Default)]
pub struct PasswordForm {
    pub url: String,
    pub username: String,
    /// The server asked for it: every request is refused until a new password is chosen.
    pub forced: bool,
    pub old: String,
    pub new: String,
    pub repeat: String,
    pub busy: bool,
    pub error: Option<String>,
    focused: bool,
}

impl PasswordForm {
    /// Checks what can be checked here (the server checks its password policy).
    fn problem(&self) -> Option<&'static str> {
        if self.old.is_empty() {
            Some(if self.forced { "Enter the temporary password you signed in with." } else { "Enter your current password." })
        } else if self.new.is_empty() {
            Some("Enter a new password.")
        } else if self.new != self.repeat {
            Some("The new passwords don't match.")
        } else if self.new == self.old {
            Some("The new password must be different from the current one.")
        } else {
            None
        }
    }
}

/// The "Add server" form (Account page and the FreedomOffice Server place).
#[derive(Default)]
pub struct AddServer {
    pub open: bool,
    /// 0: server address, 1: username and password.
    pub stage: u8,
    pub url_input: String,
    pub url: String,
    pub username: String,
    /// Typed by the user, sent once to the server, never stored.
    pub password: String,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct VersionsState {
    pub doc: CloudDoc,
    pub list: Option<Vec<VersionInfo>>,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct CloudState {
    started: bool,
    pub client_id: Option<String>,
    pub client_id_input: String,
    pub session: Option<Arc<Session>>,
    pub user: Option<User>,
    /// Cancel flag of the interactive sign-in in progress.
    signing_in: Option<Arc<AtomicBool>>,
    restoring: bool,
    pub error: Option<String>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    pub open_place: Place,
    pub save_place: Place,
    pub browser: Browser,
    pub save_name: String,
    /// The open document lives in OneDrive / SharePoint or on a FreedomOffice server.
    pub doc: Option<CloudDoc>,
    /// A downloaded document waiting for the open (after the save prompt).
    pub pending_open: Option<CloudDoc>,
    pub opening: Option<String>,
    pub uploading: bool,
    upload_again: bool,
    pub upload_failed: bool,
    pub conflict: Option<Conflict>,
    pub share: Option<ShareState>,
    pub autosave: bool,
    last_edits: u64,
    last_edit_time: f64,
    /// Signed-in FreedomOffice servers.
    pub servers: Vec<ServerEntry>,
    pub add_server: AddServer,
    pub versions: Option<VersionsState>,
    pub password: Option<PasswordForm>,
    /// Tests: local copies of cloud documents go here instead of the user's data folder.
    pub cache_override: Option<PathBuf>,
}

impl Default for CloudState {
    fn default() -> Self {
        let (tx, rx) = channel();
        CloudState {
            started: false,
            client_id: None,
            client_id_input: String::new(),
            session: None,
            user: None,
            signing_in: None,
            restoring: false,
            error: None,
            tx,
            rx,
            open_place: Place::Local,
            save_place: Place::Local,
            browser: Browser::default(),
            save_name: String::new(),
            doc: None,
            pending_open: None,
            opening: None,
            uploading: false,
            upload_again: false,
            upload_failed: false,
            conflict: None,
            share: None,
            autosave: false,
            last_edits: 0,
            last_edit_time: 0.0,
            servers: Vec::new(),
            add_server: AddServer::default(),
            versions: None,
            password: None,
            cache_override: None,
        }
    }
}

fn share_green() -> Color32 {
    if theme::is_dark() { theme::rgb(0x37A660) } else { theme::rgb(0x217346) }
}

fn error_red() -> Color32 {
    if theme::is_dark() { theme::rgb(0xFF99A4) } else { theme::rgb(0xC42B1C) }
}

/// "OneDrive - contoso.com" / "OneDrive - Personal"
fn org_label(user: Option<&User>) -> String {
    let domain = user.and_then(|u| u.email.split_once('@')).map(|(_, d)| d.to_lowercase()).unwrap_or_default();
    let personal = ["outlook.", "hotmail.", "live.", "msn.", "gmail."].iter().any(|d| domain.starts_with(d));
    if domain.is_empty() || personal {
        "Personal".into()
    } else {
        domain
    }
}

fn server_user(s: &ServerSession) -> User {
    User { name: s.account.display_name.clone(), email: s.account.username.clone() }
}

impl App {
    fn cloud_spawn(&self, f: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.cloud.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    // ------------------------------------------------------------------
    // Connections
    // ------------------------------------------------------------------

    fn server_index(&self, url: &str) -> Option<usize> {
        self.cloud.servers.iter().position(|e| e.session.account.url == url)
    }

    /// The signed-in connection for a place in Open / Save As.
    fn place_conn(&self, place: Place) -> Option<Conn> {
        match place {
            Place::Local | Place::AddServer => None,
            Place::Server(i, _) => self.cloud.servers.get(i).map(|e| Conn::Server(e.session.clone())),
            _ => self.cloud.session.clone().map(Conn::Microsoft),
        }
    }

    /// The signed-in connection for a cloud document.
    fn doc_conn(&self, doc: &CloudDoc) -> Option<Conn> {
        match &doc.provider {
            Provider::Microsoft => self.cloud.session.clone().map(Conn::Microsoft),
            Provider::FreedomServer(url) => self.server_index(url).map(|i| Conn::Server(self.cloud.servers[i].session.clone())),
        }
    }

    fn doc_server(&self) -> Option<Arc<ServerSession>> {
        match self.cloud.doc.as_ref().map(|d| &d.provider) {
            Some(Provider::FreedomServer(url)) => self.server_index(url).map(|i| self.cloud.servers[i].session.clone()),
            _ => None,
        }
    }

    /// Where documents of this connection are kept on this computer.
    fn cache_root_of(&self, conn: &Conn) -> Option<PathBuf> {
        match &self.cloud.cache_override {
            Some(d) => Some(d.join(cloud::place_name(&conn.provider()).replace(':', "_"))),
            None => conn.cache_root(),
        }
    }

    fn browser_conn(&self) -> Option<Conn> {
        self.cloud.browser.root.and_then(|p| self.place_conn(p))
    }

    fn save_servers(&mut self) {
        let accounts: Vec<server::Account> = self.cloud.servers.iter().map(|e| e.session.account.clone()).collect();
        if let Err(e) = server::save_accounts(&accounts) {
            self.status = Some(format!("Couldn't remember the server sign-in: {e}"));
        }
    }

    // ------------------------------------------------------------------
    // Frame hook: startup, results from worker threads, AutoSave
    // ------------------------------------------------------------------

    pub fn poll_cloud(&mut self, ctx: &egui::Context) {
        if !self.cloud.started {
            self.cloud.started = true;
            self.cloud.client_id = cloud::client_id();
            if let Some(id) = self.cloud.client_id.clone() {
                if cloud::token_path().is_some_and(|p| p.exists()) {
                    self.cloud.restoring = true;
                    self.cloud_spawn(move || {
                        let result = Session::restore(&id, Box::new(cloud::Http::new())).and_then(|s| match s {
                            Some(s) => s.me().map(|u| Some((Arc::new(s), u))),
                            None => Ok(None),
                        });
                        Event::Session { result, interactive: false }
                    });
                }
            }
            self.restore_servers();
            self.cloud_demo();
        }
        while let Ok(ev) = self.cloud.rx.try_recv() {
            self.cloud_event(ev);
        }
        self.server_password_check();
        // AutoSave: upload a few seconds after the last edit.
        let now = ctx.input(|i| i.time);
        if self.edits != self.cloud.last_edits {
            self.cloud.last_edits = self.edits;
            self.cloud.last_edit_time = now;
        }
        if self.cloud.autosave && self.cloud.doc.is_some() && self.dirty && self.live.is_none() {
            let idle = now - self.cloud.last_edit_time;
            if idle >= 5.0 && self.edit.is_none() && !self.cloud.uploading && self.dialog.is_none() && self.cloud.conflict.is_none() {
                self.save();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
    }

    /// Server accounts from the encrypted cache; tokens are checked in the background.
    fn restore_servers(&mut self) {
        for account in server::load_accounts() {
            let session = Arc::new(ServerSession::new(account, Box::new(cloud::Http::new())));
            self.server_check(&session);
            self.cloud.servers.push(ServerEntry { session, error: None, me: None });
        }
    }

    /// Checks the device token and gets the account's organisations (in the background).
    fn server_check(&self, session: &Arc<ServerSession>) {
        let s = session.clone();
        self.cloud_spawn(move || Event::ServerChecked { url: s.account.url.clone(), username: s.account.username.clone(), result: s.me_info() });
    }

    /// The server asked one of the accounts for a new password: ask the person.
    fn server_password_check(&mut self) {
        if self.cloud.password.is_some() {
            return;
        }
        if let Some(e) = self.cloud.servers.iter().find(|e| e.session.needs_password_change()) {
            let a = &e.session.account;
            self.cloud.password = Some(PasswordForm { url: a.url.clone(), username: a.username.clone(), forced: true, ..Default::default() });
        }
    }

    /// File > Account > Change password.
    pub(super) fn server_change_password(&mut self, i: usize) {
        if let Some(e) = self.cloud.servers.get(i) {
            let a = &e.session.account;
            let forced = e.session.needs_password_change();
            self.cloud.password = Some(PasswordForm { url: a.url.clone(), username: a.username.clone(), forced, ..Default::default() });
        }
    }

    /// Sends the password change; the typed passwords leave the form now and are dropped after
    /// the request.
    fn password_submit(&mut self) {
        let Some(f) = &mut self.cloud.password else { return };
        if f.busy {
            return;
        }
        if let Some(p) = f.problem() {
            f.error = Some(p.into());
            return;
        }
        let Some(s) = self.cloud.servers.iter().find(|e| e.session.account.url == f.url).map(|e| e.session.clone()) else {
            self.cloud.password = None;
            return;
        };
        f.busy = true;
        f.error = None;
        let (old, new) = (std::mem::take(&mut f.old), std::mem::take(&mut f.new));
        f.repeat.clear();
        let url = f.url.clone();
        self.cloud_spawn(move || {
            let result = s.change_password(&old, &new);
            drop((old, new));
            Event::PasswordChanged { url, result }
        });
    }

    /// Hidden screenshot helper: `FREEDOMOFFICE_CLOUD_DEMO=share|account|open` shows the cloud UI
    /// with a sample document and no network access.
    fn cloud_demo(&mut self) {
        let Ok(mode) = std::env::var("FREEDOMOFFICE_CLOUD_DEMO") else { return };
        match mode.as_str() {
            "share" => {
                self.cloud.user = Some(User { name: "Sample User".into(), email: "sample.user@contoso.com".into() });
                self.cloud.doc = Some(CloudDoc {
                    drive_id: "demo".into(),
                    item_id: "demo".into(),
                    name: "Budget 2026.xlsx".into(),
                    etag: String::new(),
                    web_url: String::new(),
                    parent: None,
                    local: PathBuf::from("Budget 2026.xlsx"),
                    provider: Provider::Microsoft,
                    live: None,
                });
                self.wb.path = Some(PathBuf::from("Budget 2026.xlsx"));
                self.cloud_open_share();
            }
            "account" => self.backstage = Some(Backstage::Account),
            "open" => {
                self.cloud.open_place = Place::MyFiles;
                self.backstage = Some(Backstage::Open);
            }
            _ => {}
        }
    }

    fn cloud_event(&mut self, ev: Event) {
        match ev {
            Event::Session { result, interactive } => {
                self.cloud.signing_in = None;
                self.cloud.restoring = false;
                match result {
                    Ok(Some((s, u))) => {
                        self.cloud.session = Some(s);
                        self.cloud.user = Some(u);
                        self.cloud.error = None;
                        self.cloud.browser = Browser::default();
                        if interactive {
                            self.status = Some("Signed in to Microsoft 365.".into());
                        }
                        if self.cloud.upload_again {
                            self.cloud.upload_again = false;
                            self.cloud_upload(UploadKind::Save, true);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => self.cloud.error = Some(e),
                }
            }
            Event::Listing(seq, result) => {
                if seq == self.cloud.browser.seq {
                    self.cloud.browser.loading = false;
                    match result {
                        Ok(items) => {
                            self.cloud.browser.items = items;
                            self.cloud.browser.error = None;
                        }
                        Err(e) => {
                            self.cloud.browser.items.clear();
                            self.cloud.browser.error = Some(e);
                        }
                    }
                }
            }
            Event::Opened(result) => {
                self.cloud.opening = None;
                match result {
                    Ok(doc) => {
                        let local = doc.local.clone();
                        self.cloud.pending_open = Some(doc);
                        self.backstage = None;
                        self.confirm_then(Pending::Open(Some(local)));
                    }
                    Err(e) => self.dialog = Some(Dialog::Message { title: "Open".into(), text: format!("Sorry, we couldn't open the file from the cloud.\n\n{e}") }),
                }
            }
            Event::Uploaded { kind, local, conn, result, opened } => self.cloud_uploaded(kind, local, conn, result, opened),
            Event::Invited(result) => {
                if let Some(sh) = &mut self.cloud.share {
                    sh.busy = false;
                    match result {
                        Ok(n) => {
                            sh.info = Some(if n == 1 { "Shared with 1 person.".into() } else { format!("Shared with {n} people.") });
                            sh.error = None;
                            sh.emails.clear();
                            sh.message.clear();
                            sh.invite.clear();
                            sh.user_query.clear();
                            sh.user_results.clear();
                            sh.perms = None;
                        }
                        Err(e) => sh.error = Some(e),
                    }
                }
            }
            Event::Link(result) => {
                if let Some(sh) = &mut self.cloud.share {
                    sh.busy = false;
                    match result {
                        Ok(url) => {
                            if let Some(c) = &self.ctx {
                                c.copy_text(url.clone());
                            }
                            sh.info = Some("Link copied to the clipboard.".into());
                            sh.error = None;
                            sh.link = Some(url);
                            sh.link_password.clear();
                            sh.perms = None;
                        }
                        Err(e) => sh.error = Some(e),
                    }
                }
            }
            Event::Permissions(result) => {
                if let Some(sh) = &mut self.cloud.share {
                    sh.busy = false;
                    match result {
                        Ok(p) => sh.perms = Some(p),
                        Err(e) => {
                            sh.perms = Some(Vec::new());
                            sh.error = Some(e);
                        }
                    }
                }
            }
            Event::ServerSignedIn(result) => {
                let a = &mut self.cloud.add_server;
                a.busy = false;
                match result {
                    Ok((session, me)) => {
                        let host = session.host();
                        let (url, username) = (session.account.url.clone(), session.account.username.clone());
                        let entry = ServerEntry { session, error: None, me };
                        let i = match self.cloud.servers.iter().position(|e| e.session.account.url == url && e.session.account.username == username) {
                            Some(i) => {
                                self.cloud.servers[i] = entry;
                                i
                            }
                            None => {
                                // One account per server: signing in as someone else replaces it.
                                self.cloud.servers.retain(|e| e.session.account.url != url);
                                self.cloud.servers.push(entry);
                                self.cloud.servers.len() - 1
                            }
                        };
                        self.cloud.add_server = AddServer::default();
                        self.save_servers();
                        self.status = Some(format!("Signed in to {host}."));
                        for p in [&mut self.cloud.open_place, &mut self.cloud.save_place] {
                            if matches!(*p, Place::AddServer | Place::Server(..)) {
                                *p = Place::Server(i, ServerView::MyFiles);
                            }
                        }
                        self.cloud.browser.root = None;
                        if self.cloud.upload_again {
                            self.cloud.upload_again = false;
                            self.cloud_upload(UploadKind::Save, true);
                        }
                    }
                    Err(e) => a.error = Some(e),
                }
            }
            Event::ServerChecked { url, username, result } => {
                let Some(entry) = self.cloud.servers.iter_mut().find(|x| x.session.account.url == url && x.session.account.username == username) else { return };
                match result {
                    Ok(me) => {
                        // A view of an organisation that is gone shows My files.
                        let n = me.orgs.len();
                        let changed = entry.me.as_ref().is_some_and(|m| m.orgs != me.orgs);
                        entry.me = Some(me);
                        entry.error = None;
                        for p in [&mut self.cloud.open_place, &mut self.cloud.save_place] {
                            if let Place::Server(i, ServerView::Org(k)) = *p
                                && self.cloud.servers.get(i).is_some_and(|e| e.session.account.url == url)
                                && (k >= n || changed)
                            {
                                *p = Place::Server(i, ServerView::MyFiles);
                                self.cloud.browser.root = None;
                            }
                        }
                    }
                    // Offline is fine (the token is checked again on use); a rejected token isn't.
                    Err(e) if e.contains("expired or was revoked") => entry.error = Some(e),
                    Err(_) => {}
                }
            }
            Event::PasswordChanged { url, result } => {
                let Some(f) = &mut self.cloud.password else { return };
                if f.url != url {
                    return;
                }
                f.busy = false;
                match result {
                    Ok(n) => {
                        self.cloud.password = None;
                        self.status = Some(match n {
                            0 => "Your password was changed.".into(),
                            1 => "Your password was changed. Your other device was signed out.".into(),
                            n => format!("Your password was changed. Your {n} other devices were signed out."),
                        });
                        // Everything the temporary password blocked works now.
                        if let Some(s) = self.cloud.servers.iter().find(|e| e.session.account.url == url).map(|e| e.session.clone()) {
                            self.server_check(&s);
                        }
                        self.cloud.browser.root = None;
                    }
                    Err(e) => {
                        // The typed passwords are gone: start again in the first box.
                        f.error = Some(e);
                        f.focused = false;
                    }
                }
            }
            Event::OrgShared(result) => {
                if let Some(sh) = &mut self.cloud.share {
                    sh.busy = false;
                    match result {
                        Ok(name) => {
                            sh.info = Some(format!("Shared with everyone in {name}."));
                            sh.error = None;
                            sh.perms = None;
                        }
                        Err(e) => sh.error = Some(e),
                    }
                }
            }
            Event::Users(seq, result) => {
                if let Some(sh) = &mut self.cloud.share {
                    if seq == sh.user_seq {
                        match result {
                            Ok(list) => sh.user_results = list,
                            Err(e) => sh.error = Some(e),
                        }
                    }
                }
            }
            Event::FolderCreated(result) => match result {
                Ok(item) => {
                    self.cloud.browser.new_folder = None;
                    self.status = Some(format!("Created the folder {}.", item.name));
                    if let Some((_, t)) = self.cloud.browser.crumbs.last().cloned() {
                        self.cloud_navigate(t, String::new(), false);
                    }
                }
                Err(e) => self.cloud.browser.error = Some(e),
            },
            Event::Versions(result) => {
                if let Some(v) = &mut self.cloud.versions {
                    v.busy = false;
                    match result {
                        Ok(list) => v.list = Some(list),
                        Err(e) => {
                            v.list = Some(Vec::new());
                            v.error = Some(e);
                        }
                    }
                }
            }
            Event::VersionRestored(result) => match result {
                Ok(item) => {
                    self.cloud.versions = None;
                    self.status = Some("Version restored. Opening it...".into());
                    let conn = self.cloud.doc.as_ref().and_then(|d| self.doc_conn(d));
                    if let Some(conn) = conn {
                        self.cloud_open_with(conn, item);
                    }
                }
                Err(e) => {
                    if let Some(v) = &mut self.cloud.versions {
                        v.busy = false;
                        v.error = Some(e);
                    }
                }
            },
            Event::VersionCopy(result) => match result {
                Ok(path) => {
                    self.cloud.versions = None;
                    self.confirm_then(Pending::Open(Some(path)));
                }
                Err(e) => {
                    if let Some(v) = &mut self.cloud.versions {
                        v.busy = false;
                        v.error = Some(e);
                    }
                }
            },
        }
    }

    fn cloud_uploaded(&mut self, kind: UploadKind, local: PathBuf, conn: Conn, result: Result<DriveItem, UploadError>, opened: Option<Result<CloudDoc, String>>) {
        self.cloud.uploading = false;
        let place = cloud::place_name(&conn.provider());
        // A new server file joins its live session at once, as if opened from the server: the
        // downloaded copy is exactly what was uploaded (else someone changed it in between and
        // it is joined when reopened).
        let joinable = match (&result, opened) {
            (Ok(item), Some(Ok(doc))) if doc.live.is_some() && server::version_of_etag(&doc.etag) == server::version_of_etag(&item.etag) => Some(doc),
            _ => None,
        };
        if let (Some(doc), UploadKind::Copy | UploadKind::New { .. }) = (joinable, &kind) {
            if matches!(kind, UploadKind::New { .. }) && doc.local != local {
                // The Save As copy is replaced by the server's (in the folder Open uses).
                let _ = std::fs::remove_file(&local);
                for d in local.ancestors().skip(1).take(2) {
                    let _ = std::fs::remove_dir(d);
                }
            }
            self.live_leave(false);
            self.wb.path = Some(doc.local.clone());
            let name = doc.name.clone();
            self.cloud.doc = Some(doc);
            self.cloud.upload_failed = false;
            self.live_reset();
            self.status = Some(format!("Saved to {place} as {name}. Connecting for live editing..."));
            if self.cloud.upload_again {
                self.cloud.upload_again = false;
            }
            return;
        }
        match (kind, result) {
            (UploadKind::Save, Ok(item)) => {
                if let Some(doc) = &mut self.cloud.doc {
                    doc.update_from(&item);
                }
                self.cloud.upload_failed = false;
                self.status = Some(format!("Saved to {place}."));
            }
            (UploadKind::Copy | UploadKind::New { .. }, Ok(item)) => {
                let mut doc = conn.doc_from(&item, local);
                if doc.drive_id.is_empty() && doc.provider == Provider::Microsoft {
                    doc.drive_id = self.cloud.doc.as_ref().map(|d| d.drive_id.clone()).unwrap_or_default();
                }
                self.status = Some(format!("Saved to {place} as {}.", doc.name));
                self.cloud.doc = Some(doc);
                self.cloud.upload_failed = false;
            }
            (UploadKind::Save, Err(UploadError::Conflict)) => {
                self.cloud.upload_failed = true;
                self.cloud.conflict = Some(Conflict::Changed);
            }
            (UploadKind::New { folder, name }, Err(UploadError::Exists)) => {
                self.cloud.upload_failed = true;
                self.cloud.conflict = Some(Conflict::Exists { conn, folder, name, local });
            }
            (_, Err(e)) => {
                self.cloud.upload_failed = true;
                self.status = Some(format!("Couldn't save to {place}: {e} Your changes are saved on this PC."));
                if !self.cloud.autosave {
                    self.dialog = Some(Dialog::Message {
                        title: "Save".into(),
                        text: format!("Couldn't upload to {place}.\n\n{e}\n\nYour changes are saved on this PC in\n{}", local.display()),
                    });
                }
            }
        }
        if self.cloud.upload_again {
            self.cloud.upload_again = false;
            self.cloud_upload(UploadKind::Save, true);
        }
    }

    // ------------------------------------------------------------------
    // Sign-in
    // ------------------------------------------------------------------

    pub fn cloud_sign_in(&mut self) {
        let Some(id) = self.cloud.client_id.clone() else {
            self.backstage = Some(Backstage::Account);
            return;
        };
        if self.cloud.signing_in.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cloud.signing_in = Some(cancel.clone());
        self.cloud.error = None;
        self.cloud_spawn(move || {
            let result = Session::sign_in(&id, Box::new(cloud::Http::new()), &cancel).and_then(|s| s.me().map(|u| Some((Arc::new(s), u))));
            Event::Session { result, interactive: true }
        });
    }

    pub fn cloud_sign_out(&mut self) {
        cloud::auth::delete_token();
        self.cloud.session = None;
        self.cloud.user = None;
        self.cloud.browser = Browser::default();
        self.cloud.share = None;
        self.status = Some("Signed out.".into());
    }

    /// Sends the typed credentials to the server (HTTPS, or http://localhost) for a device token.
    fn server_sign_in(&mut self) {
        let a = &mut self.cloud.add_server;
        if a.busy || a.username.trim().is_empty() || a.password.is_empty() {
            return;
        }
        a.busy = true;
        a.error = None;
        let (url, user) = (a.url.clone(), a.username.trim().to_string());
        let password = std::mem::take(&mut a.password);
        self.cloud_spawn(move || {
            let result = ServerSession::login(Box::new(cloud::Http::new()), &url, &user, &password, &server::device_name());
            drop(password);
            // Also who this is (organisations); a temporary password is noticed here too.
            Event::ServerSignedIn(result.map(|s| {
                let me = s.me_info().ok();
                (Arc::new(s), me)
            }))
        });
    }

    /// Signs out of a server: revokes the device token there and forgets it here.
    fn server_sign_out(&mut self, i: usize) {
        if i >= self.cloud.servers.len() {
            return;
        }
        let entry = self.cloud.servers.remove(i);
        let host = entry.session.host();
        let s = entry.session.clone();
        std::thread::spawn(move || {
            let _ = s.logout();
        });
        self.save_servers();
        for p in [&mut self.cloud.open_place, &mut self.cloud.save_place] {
            if let Place::Server(j, _) = *p {
                *p = if j == i { Place::Local } else if j > i { Place::Server(j - 1, ServerView::MyFiles) } else { *p };
            }
        }
        self.cloud.browser = Browser::default();
        self.status = Some(format!("Signed out of {host}."));
    }

    // ------------------------------------------------------------------
    // Saving
    // ------------------------------------------------------------------

    fn place_of_doc(&self, doc: &CloudDoc) -> Place {
        match &doc.provider {
            Provider::Microsoft => Place::MyFiles,
            Provider::FreedomServer(url) => self.server_index(url).map(|i| Place::Server(i, ServerView::MyFiles)).unwrap_or(Place::AddServer),
        }
    }

    /// Ctrl+S on a cloud document: write the local copy, then upload in the background.
    pub fn cloud_save(&mut self) -> bool {
        let Some(doc) = self.cloud.doc.clone() else { return false };
        if let Some(why) = self.live.as_ref().filter(|l| !l.editor).and_then(|l| l.stopped.clone()) {
            // Access ended or became view-only with changes the server refused: only a copy
            // can keep what is in this window.
            self.status = Some(why);
            self.live_save_copy();
            return false;
        }
        if self.live.is_some() {
            // Live: every change is already on the server; Save stores a version now.
            self.live_flush();
            if self.live.as_ref().is_some_and(|l| l.sess.has_pending() && !l.online) {
                // Offline: keep the local copy; the changes are sent when the connection is back.
                let saved = crate::io::save(&self.wb, &doc.local).is_ok();
                self.status = Some(format!(
                    "Not connected to {}. Your latest changes are {}kept on this PC and are sent when the connection is back.",
                    cloud::place_name(&doc.provider),
                    if saved { "" } else { "not " }
                ));
                return saved;
            }
            self.live_snapshot();
            if let Some(t) = self.live_title() {
                self.status = Some(t.trim_start_matches(" - ").to_string());
            }
            return true;
        }
        let native = self.wb.path.as_ref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| e == "xlsx" || e == "xlsm");
        if !native {
            // .xls / .csv / .ods can't be written back: save a workbook copy instead.
            let stem = doc.name.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(doc.name.clone());
            self.cloud.save_name = format!("{stem}.xlsx");
            let place = self.place_of_doc(&doc);
            self.cloud.save_place = place;
            if let Some(parent) = &doc.parent {
                self.cloud.browser = Browser { root: Some(place), ..Default::default() };
                self.cloud_navigate(Target::Folder(parent.clone()), "Folder".into(), true);
            }
            self.backstage = Some(Backstage::SaveAs);
            return false;
        }
        match crate::io::save(&self.wb, &doc.local) {
            Ok(()) => {
                self.wb.path = Some(doc.local.clone());
                self.dirty = false;
                self.remove_recovery();
                self.cloud_upload(UploadKind::Save, true);
                true
            }
            Err(e) => {
                self.dialog = Some(Dialog::Message { title: "Save".into(), text: format!("Couldn't save the file.\n\n{e}") });
                false
            }
        }
    }

    /// Uploads the open document's local copy. `check` sends the eTag (no silent overwrite).
    fn cloud_upload(&mut self, kind: UploadKind, check: bool) {
        let Some(doc) = self.cloud.doc.clone() else { return };
        if self.cloud.uploading {
            self.cloud.upload_again = true;
            return;
        }
        let place = cloud::place_name(&doc.provider);
        let Some(conn) = self.doc_conn(&doc) else {
            if doc.provider == Provider::Microsoft && self.cloud.restoring {
                self.cloud.upload_again = true;
            } else if doc.provider == Provider::Microsoft {
                self.cloud.upload_failed = true;
                self.status = Some("Saved on this PC. Sign in (File > Account) to save to OneDrive.".into());
            } else {
                self.cloud.upload_failed = true;
                self.status = Some(format!("Saved on this PC. Sign in to {place} (File > Account) to save to the server."));
            }
            return;
        };
        let data = match std::fs::read(&doc.local) {
            Ok(d) => d,
            Err(e) => {
                self.status = Some(format!("Couldn't read {}: {e}", doc.local.display()));
                return;
            }
        };
        self.cloud.uploading = true;
        self.status = Some(format!("Saving to {place} ({})...", size_label(data.len())));
        let root = self.cache_root_of(&conn);
        self.cloud_spawn(move || {
            let result = match &kind {
                UploadKind::Save => conn.upload(&doc, check.then_some(doc.etag.as_str()), &data),
                UploadKind::Copy => match &doc.parent {
                    Some(parent) => conn.upload_new(parent, &graph::copy_name(&doc.name), &data, "rename"),
                    None => Err(UploadError::Failed("The file's folder is unknown.".into())),
                },
                UploadKind::New { folder, name } => conn.upload_new(folder, name, &data, if check { "fail" } else { "replace" }),
            };
            let opened = if matches!(kind, UploadKind::Save) { None } else { reopen_new(&conn, root.as_deref(), &result) };
            Event::Uploaded { kind, local: doc.local.clone(), conn, result, opened }
        });
    }

    /// Save As into the folder shown in the cloud browser.
    fn cloud_save_as(&mut self, conn: Conn, folder: String, name: String, replace: bool) {
        self.commit_edit(false);
        let mut name = name.trim().to_string();
        if !name.to_lowercase().ends_with(".xlsx") && !name.to_lowercase().ends_with(".xlsm") {
            name.push_str(".xlsx");
        }
        let Some(dir) = self.cache_root_of(&conn).map(|d| d.join(format!("new-{}", cloud::auth::now_secs()))) else {
            self.status = Some(cloud::NO_SETTINGS_DIR.to_string());
            return;
        };
        let local = cloud::local_copy_path(&dir, "", &name);
        if let Some(d) = local.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Err(e) = crate::io::save(&self.wb, &local) {
            self.dialog = Some(Dialog::Message { title: "Save As".into(), text: format!("Couldn't save the file.\n\n{e}") });
            return;
        }
        self.wb.path = Some(local.clone());
        self.dirty = false;
        self.remove_recovery();
        self.cloud.doc = None;
        self.backstage = None;
        self.start_new_upload(conn, folder, name, local, replace);
    }

    fn start_new_upload(&mut self, conn: Conn, folder: String, name: String, local: PathBuf, replace: bool) {
        let data = match std::fs::read(&local) {
            Ok(d) => d,
            Err(e) => {
                self.status = Some(format!("Couldn't read {}: {e}", local.display()));
                return;
            }
        };
        self.cloud.uploading = true;
        self.status = Some(format!("Saving {name} to {}...", cloud::place_name(&conn.provider())));
        let root = self.cache_root_of(&conn);
        self.cloud_spawn(move || {
            let result = conn.upload_new(&folder, &name, &data, if replace { "replace" } else { "fail" });
            let opened = reopen_new(&conn, root.as_deref(), &result);
            Event::Uploaded { kind: UploadKind::New { folder, name }, local, conn, result, opened }
        });
    }

    /// Title bar suffix like "Saved to OneDrive" or "Saved to files.example.com".
    pub fn cloud_title_suffix(&self) -> String {
        if let Some(t) = self.live_title() {
            return t;
        }
        match &self.cloud.doc {
            None => String::new(),
            Some(_) if self.cloud.uploading => " - Saving...".into(),
            Some(d) if self.cloud.upload_failed => format!(" - Not saved to {}", cloud::place_name(&d.provider)),
            Some(d) if !self.dirty => format!(" - Saved to {}", cloud::place_name(&d.provider)),
            Some(_) => String::new(),
        }
    }

    // ------------------------------------------------------------------
    // Browsing
    // ------------------------------------------------------------------

    fn cloud_navigate(&mut self, target: Target, label: String, push: bool) {
        let b = &mut self.cloud.browser;
        if push {
            b.crumbs.push((label, target.clone()));
        }
        b.seq += 1;
        b.loading = true;
        b.error = None;
        b.items.clear();
        b.new_folder = None;
        let seq = b.seq;
        let Some(conn) = self.browser_conn() else { return };
        self.cloud_spawn(move || Event::Listing(seq, conn.list(&target)));
    }

    fn cloud_open_with(&mut self, conn: Conn, item: DriveItem) {
        let Some(root) = self.cache_root_of(&conn) else {
            self.status = Some(cloud::NO_SETTINGS_DIR.to_string());
            return;
        };
        self.cloud.opening = Some(item.name.clone());
        self.status = Some(format!("Downloading {}...", item.name));
        self.cloud_spawn(move || Event::Opened(conn.open(&item, &root)));
    }

    fn cloud_open_item(&mut self, item: DriveItem) {
        if let Some(conn) = self.browser_conn() {
            self.cloud_open_with(conn, item);
        }
    }

    /// Places column for File > Open / Save As. Draws the cloud browser for cloud places and
    /// returns true when the local place is selected (the caller draws that page).
    pub fn cloud_places_ui(&mut self, ui: &mut egui::Ui, save_as: bool) -> bool {
        let org = org_label(self.cloud.user.as_ref());
        let mut places: Vec<(Place, char, &'static str, String)> = vec![
            (Place::Local, if save_as { PC } else { CLOCK }, if save_as { "🖥" } else { "🕘" }, if save_as { "This PC".into() } else { "Recent".into() }),
            (Place::MyFiles, CLOUD, "☁", format!("OneDrive - {org}")),
        ];
        if !save_as {
            places.push((Place::Recent, CLOCK, "🕘", "Recent in OneDrive".into()));
        }
        places.push((Place::Shared, PEOPLE, "👥", "Shared with me".into()));
        places.push((Place::Sites, GLOBE, "🌐", format!("Sites - {org}")));
        if self.cloud.servers.is_empty() {
            places.push((Place::AddServer, SERVER, "🖧", "FreedomOffice Server".into()));
        }
        for (i, e) in self.cloud.servers.iter().enumerate() {
            places.push((Place::Server(i, ServerView::MyFiles), SERVER, "🖧", format!("FreedomOffice - {}", e.session.host())));
        }
        let current = if save_as { self.cloud.save_place } else { self.cloud.open_place };
        let mut picked = None;
        ui.vertical(|ui| {
            ui.set_width(240.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            for (place, code, fallback, label) in places {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(230.0, 40.0), Sense::click());
                if place.same_row(current) {
                    ui.painter().rect_filled(rect, 3.0, theme::accent_light());
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, theme::hover());
                }
                let (g, is_icon) = self.icon(code, fallback);
                let font = if is_icon { theme::icon_font(17.0) } else { theme::ui_font(16.0) };
                ui.painter().text(Pos2::new(rect.left() + 20.0, rect.center().y), Align2::CENTER_CENTER, g, font, theme::accent_dark());
                let clip = ui.painter().with_clip_rect(rect.shrink(2.0));
                clip.text(Pos2::new(rect.left() + 42.0, rect.center().y), Align2::LEFT_CENTER, label, theme::ui_font(13.5), theme::text());
                if resp.clicked() && !place.same_row(current) {
                    picked = Some(place);
                }
            }
        });
        if let Some(p) = picked {
            if save_as { self.cloud.save_place = p } else { self.cloud.open_place = p }
            self.cloud.browser.root = None;
        }
        let place = if save_as { self.cloud.save_place } else { self.cloud.open_place };
        if place == Place::Local {
            return true;
        }
        ui.add_space(24.0);
        ui.vertical(|ui| self.cloud_browser_ui(ui, place, save_as));
        false
    }

    fn signed_out_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if self.cloud.client_id.is_none() {
            ui.label(RichText::new("Connect FreedomOffice Sheets to Microsoft 365").size(17.0).color(theme::text()));
            ui.add_space(6.0);
            ui.label(RichText::new("To open and save files in OneDrive and SharePoint, first register the app with your Microsoft account.").color(theme::text_dim()));
            ui.add_space(10.0);
            if ui.add(egui::Button::new(RichText::new("Set up in Account").size(14.0)).min_size(Vec2::new(160.0, 32.0))).clicked() {
                self.backstage = Some(Backstage::Account);
            }
            return;
        }
        ui.label(RichText::new("Sign in to open and save files in OneDrive and SharePoint").size(17.0).color(theme::text()));
        ui.add_space(10.0);
        self.sign_in_button(ui);
    }

    fn sign_in_button(&mut self, ui: &mut egui::Ui) {
        if self.cloud.restoring {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Signing in...");
            });
        } else if let Some(cancel) = self.cloud.signing_in.clone() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Finish signing in in your browser...");
                if ui.button("Cancel").clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
            });
        } else {
            let b = egui::Button::new(RichText::new("Sign in").size(14.0).color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(120.0, 32.0));
            if ui.add(b).clicked() {
                self.cloud_sign_in();
            }
        }
        if let Some(e) = &self.cloud.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(theme::rgb(0xC42B1C)));
        }
    }

    fn cloud_browser_ui(&mut self, ui: &mut egui::Ui, place: Place, save_as: bool) {
        if place == Place::AddServer {
            ui.label(RichText::new("Connect to a FreedomOffice server").size(17.0).color(theme::text()));
            ui.add_space(6.0);
            ui.label(RichText::new("Open, save and share workbooks on your own document server, without a Microsoft account.").color(theme::text_dim()));
            ui.add_space(10.0);
            self.cloud.add_server.open = true;
            self.add_server_ui(ui);
            return;
        }
        if let Place::Server(i, _) = place {
            match self.cloud.servers.get(i) {
                None => return,
                Some(e) if e.error.is_some() => {
                    let (msg, url, user) = (e.error.clone().unwrap_or_default(), e.session.account.url.clone(), e.session.account.username.clone());
                    ui.add_space(8.0);
                    ui.label(RichText::new(msg).color(error_red()));
                    ui.add_space(8.0);
                    if ui.add(egui::Button::new("Sign in again").min_size(Vec2::new(130.0, 28.0))).clicked() {
                        self.cloud.add_server = AddServer { open: true, stage: 1, url_input: url.clone(), url, username: user, ..Default::default() };
                        self.backstage = Some(Backstage::Account);
                    }
                    return;
                }
                Some(_) => {}
            }
        } else if self.cloud.session.is_none() {
            self.signed_out_ui(ui);
            return;
        }
        if self.cloud.browser.root != Some(place) {
            let (root, label) = self.place_root(place);
            self.cloud.browser = Browser { root: Some(place), seq: self.cloud.browser.seq, ..Default::default() };
            self.cloud_navigate(root, label, true);
        }
        // Server views: My files / Shared with me / each organisation / Recent.
        if let Place::Server(i, view) = place {
            let mut pick = None;
            let mut views: Vec<(ServerView, String)> = vec![(ServerView::MyFiles, "My files".into()), (ServerView::Shared, "Shared with me".into())];
            if let Some(e) = self.cloud.servers.get(i) {
                views.extend(e.orgs().iter().enumerate().map(|(k, o)| (ServerView::Org(k), o.name.clone())));
            }
            if !save_as {
                views.push((ServerView::Recent, "Recent".into()));
            }
            ui.horizontal_wrapped(|ui| {
                for (v, label) in views {
                    let r = ui.selectable_label(view == v, RichText::new(label).size(13.5));
                    let r = if matches!(v, ServerView::Org(_)) { r.on_hover_text("Your organisation's folder") } else { r };
                    if r.clicked() && view != v {
                        pick = Some(v);
                    }
                }
            });
            if let Some(v) = pick {
                let p = Place::Server(i, v);
                if save_as { self.cloud.save_place = p } else { self.cloud.open_place = p }
            }
            ui.add_space(6.0);
        }
        // Breadcrumb
        let mut go: Option<usize> = None;
        let mut refresh = false;
        let conn = self.browser_conn();
        let folder = self.cloud.browser.crumbs.last().and_then(|(_, t)| conn.as_ref().and_then(|c| c.folder_of(t)));
        let is_server = matches!(place, Place::Server(..));
        let mut new_folder = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let n = self.cloud.browser.crumbs.len();
            for (i, (label, _)) in self.cloud.browser.crumbs.iter().enumerate() {
                let last = i + 1 == n;
                let t = RichText::new(label).size(if last { 17.0 } else { 15.0 }).color(if last { theme::text() } else { theme::accent_dark() });
                if ui.add(egui::Button::new(t).frame(false)).clicked() && !last {
                    go = Some(i);
                }
                if !last {
                    ui.label(RichText::new("›").size(16.0).color(theme::text_dim()));
                }
            }
            ui.add_space(8.0);
            if ui.add(egui::Button::new(RichText::new("⟳").size(15.0)).frame(false)).on_hover_text("Refresh").clicked() {
                refresh = true;
            }
            if is_server && folder.is_some() && self.cloud.browser.new_folder.is_none() {
                ui.add_space(8.0);
                if ui.add(egui::Button::new(RichText::new("+ New folder").size(13.0).color(theme::accent_dark())).frame(false)).clicked() {
                    new_folder = true;
                }
            }
        });
        if new_folder {
            self.cloud.browser.new_folder = Some(String::new());
        }
        if let Some(i) = go {
            self.cloud.browser.crumbs.truncate(i + 1);
            refresh = true;
        }
        if refresh {
            if let Some((_, t)) = self.cloud.browser.crumbs.last().cloned() {
                self.cloud_navigate(t, String::new(), false);
            }
        }
        // New folder box (servers).
        if let (Some(mut name), Some(f), Some(Conn::Server(s))) = (self.cloud.browser.new_folder.clone(), folder.clone(), conn.clone()) {
            let mut create = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Folder name").desired_width(260.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    create = true;
                }
                if ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Create")).clicked() {
                    create = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
            self.cloud.browser.new_folder = if cancel { None } else { Some(name.clone()) };
            if create && !name.trim().is_empty() {
                self.cloud_spawn(move || Event::FolderCreated(s.create_folder(&f, &name)));
            }
        }
        ui.add_space(8.0);
        if save_as {
            ui.horizontal(|ui| {
                if self.cloud.save_name.is_empty() {
                    self.cloud.save_name = self.default_cloud_name();
                }
                ui.add(egui::TextEdit::singleline(&mut self.cloud.save_name).desired_width(320.0).font(theme::ui_font(13.5)));
                ui.label(RichText::new("Excel Workbook (*.xlsx)").color(theme::text_dim()));
                let ok = folder.is_some() && !self.cloud.save_name.trim().is_empty() && !self.cloud.uploading;
                let b = egui::Button::new(RichText::new("Save").size(14.0)).min_size(Vec2::new(90.0, 28.0)).fill(theme::accent_light());
                if ui.add_enabled(ok, b).on_disabled_hover_text("Open a folder to save in").clicked() {
                    if let Some(c) = conn.clone() {
                        let (f, n) = (folder.clone().unwrap_or_default(), self.cloud.save_name.clone());
                        self.cloud_save_as(c, f, n, false);
                    }
                }
            });
            if folder.is_none() {
                ui.label(RichText::new("Open a folder to save in.").size(12.0).color(theme::text_dim()));
            }
            ui.add_space(8.0);
        }
        if let Some(n) = &self.cloud.opening {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label(format!("Opening {n}..."));
            });
        }
        // Column headers
        let w = ui.available_width().clamp(420.0, 900.0);
        let (c1, c2) = (w * 0.55, w * 0.75);
        let (hr, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
        let p = ui.painter();
        for (x, t) in [(42.0, "Name"), (c1 + 6.0, "Date modified"), (c2 + 6.0, "Modified by")] {
            p.text(Pos2::new(hr.left() + x, hr.center().y), Align2::LEFT_CENTER, t, theme::ui_font(12.0), theme::text_dim());
        }
        p.line_segment([hr.left_bottom(), hr.right_bottom()], Stroke::new(1.0, theme::border()));
        if self.cloud.browser.loading {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Loading...");
            });
            return;
        }
        if let Some(e) = &self.cloud.browser.error {
            ui.add_space(10.0);
            ui.label(RichText::new(e).color(theme::rgb(0xC42B1C)));
            return;
        }
        if self.cloud.browser.items.is_empty() {
            ui.add_space(10.0);
            ui.label(RichText::new("There are no spreadsheets here.").color(theme::text_dim()));
            return;
        }
        let mut clicked: Option<DriveItem> = None;
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for it in &self.cloud.browser.items {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 34.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, theme::hover());
                }
                let (code, fb) = match it.kind {
                    Kind::Folder => (ic::FOLDER, "📁"),
                    Kind::Site => (GLOBE, "🌐"),
                    Kind::Library => (ic::FOLDER, "📚"),
                    Kind::File => (ic::TABLE, "▦"),
                };
                let (g, is_icon) = self.icon(code, fb);
                let font = if is_icon { theme::icon_font(16.0) } else { theme::ui_font(15.0) };
                let p = ui.painter();
                p.text(Pos2::new(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, g, font, if it.kind == Kind::File { theme::accent() } else { theme::text_dim() });
                let clip = p.with_clip_rect(egui::Rect::from_min_max(rect.min, Pos2::new(rect.left() + c1 - 8.0, rect.bottom())));
                clip.text(Pos2::new(rect.left() + 42.0, rect.center().y), Align2::LEFT_CENTER, &it.name, theme::ui_font(13.5), theme::text());
                p.text(Pos2::new(rect.left() + c1 + 6.0, rect.center().y), Align2::LEFT_CENTER, graph::short_date(&it.modified), theme::ui_font(12.5), theme::text_dim());
                let clip = p.with_clip_rect(egui::Rect::from_min_max(Pos2::new(rect.left() + c2, rect.top()), rect.max));
                clip.text(Pos2::new(rect.left() + c2 + 6.0, rect.center().y), Align2::LEFT_CENTER, &it.modified_by, theme::ui_font(12.5), theme::text_dim());
                if resp.clicked() {
                    clicked = Some(it.clone());
                }
            }
        });
        if let Some(it) = clicked {
            match conn.as_ref().and_then(|c| c.target_of(&it)) {
                Some(t) => self.cloud_navigate(t, it.name.clone(), true),
                None if save_as => self.cloud.save_name = it.name.clone(),
                None => {
                    if self.cloud.opening.is_none() {
                        self.cloud_open_item(it);
                    }
                }
            }
        }
    }

    /// What a place lists first, and its breadcrumb label.
    pub(super) fn place_root(&self, place: Place) -> (Target, String) {
        if let Place::Server(i, ServerView::Org(k)) = place {
            if let Some(o) = self.cloud.servers.get(i).and_then(|e| e.orgs().get(k)) {
                return (Target::Folder(o.root.clone()), o.name.clone());
            }
        }
        let label = match place {
            Place::Recent | Place::Server(_, ServerView::Recent) => "Recent",
            Place::Shared | Place::Server(_, ServerView::Shared) => "Shared with me",
            Place::Sites => "Sites",
            _ => "My files",
        };
        (place.target().unwrap_or(Target::MyFiles), label.into())
    }

    fn default_cloud_name(&self) -> String {
        let stem = self.wb.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Book1".into());
        format!("{stem}.xlsx")
    }

    // ------------------------------------------------------------------
    // Account page
    // ------------------------------------------------------------------

    pub fn account_page_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Account").size(28.0).color(theme::text()));
        ui.add_space(16.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(720.0);
            if self.cloud.client_id.is_none() {
                self.setup_ui(ui);
            } else {
                self.microsoft_account_ui(ui);
            }
            ui.add_space(28.0);
            self.server_accounts_ui(ui);
        });
    }

    fn microsoft_account_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("User Information").size(17.0).color(theme::text()));
        ui.add_space(10.0);
        match self.cloud.user.clone() {
            Some(u) => {
                ui.horizontal(|ui| {
                    initials_badge(ui, &u.initials(), 52.0);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&u.name).size(16.0).color(theme::text()));
                        ui.label(RichText::new(&u.email).color(theme::text_dim()));
                    });
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("Sign out").min_size(Vec2::new(110.0, 28.0))).clicked() {
                        self.cloud_sign_out();
                    }
                    if ui.add(egui::Button::new("Switch account").min_size(Vec2::new(130.0, 28.0))).clicked() {
                        self.cloud_sign_out();
                        self.cloud_sign_in();
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new(if cfg!(windows) { "You stay signed in on this PC; the sign-in is stored encrypted for your Windows account." } else { "You stay signed in on this computer; the sign-in is stored in a file only your user account can read." }).size(12.0).color(theme::text_dim()));
            }
            None => {
                ui.label(RichText::new("Sign in with your work, school or personal Microsoft account to open, save and share files in OneDrive and SharePoint.").color(theme::text_dim()));
                ui.add_space(10.0);
                self.sign_in_button(ui);
            }
        }
        ui.add_space(24.0);
        ui.label(RichText::new("App registration").size(15.0).color(theme::text()));
        ui.add_space(4.0);
        let id = self.cloud.client_id.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Client ID: {id}")).size(12.5).color(theme::text_dim()));
            if std::env::var("FREEDOMOFFICE_CLIENT_ID").map(|v| !v.trim().is_empty()).unwrap_or(false) {
                ui.label(RichText::new("(from FREEDOMOFFICE_CLIENT_ID)").size(12.0).color(theme::text_dim()));
            } else if self.cloud.user.is_none() && ui.add(egui::Button::new(RichText::new("Change").size(12.5)).frame(false)).clicked() {
                self.cloud.client_id_input = id.clone();
                self.cloud.client_id = None;
            }
        });
    }

    fn setup_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Connect to Microsoft 365").size(17.0).color(theme::text()));
        ui.add_space(6.0);
        ui.label(
            RichText::new("FreedomOffice Sheets signs in with an app registration that you create once in Microsoft Entra ID (free; needs no secret).")
                .color(theme::text_dim()),
        );
        ui.add_space(10.0);
        for (i, step) in SETUP_STEPS.iter().enumerate() {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(format!("{}.", i + 1)).strong().color(theme::accent_dark()));
                ui.add(egui::Label::new(RichText::new(*step).color(theme::text())).wrap());
            });
            ui.add_space(4.0);
        }
        if ui.add(egui::Button::new(RichText::new("Open the Entra admin center").color(theme::accent_dark())).frame(false)).clicked() {
            cloud::open_in_browser(ENTRA_URL);
        }
        ui.add_space(12.0);
        ui.label("Application (client) ID:");
        let mut save = false;
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.cloud.client_id_input).hint_text("00000000-0000-0000-0000-000000000000").desired_width(320.0));
            save = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = cloud::valid_client_id(self.cloud.client_id_input.trim());
            if ui.add_enabled(valid, egui::Button::new("Save").min_size(Vec2::new(80.0, 24.0)).fill(theme::accent_light())).clicked() {
                save = true;
            }
            save &= valid;
        });
        if !self.cloud.client_id_input.trim().is_empty() && !cloud::valid_client_id(self.cloud.client_id_input.trim()) {
            ui.label(RichText::new("A client ID looks like 00000000-0000-0000-0000-000000000000.").size(12.0).color(theme::text_dim()));
        }
        if let Some(p) = cloud::config_path() {
            ui.label(RichText::new(format!("Saved in {}", p.display())).size(12.0).color(theme::text_dim()));
        }
        if save {
            let id = self.cloud.client_id_input.trim().to_string();
            match cloud::save_client_id(&id) {
                Ok(()) => {
                    self.cloud.client_id = Some(id);
                    self.cloud.error = None;
                }
                Err(e) => self.cloud.error = Some(format!("Couldn't save the client ID: {e}")),
            }
        }
        if let Some(e) = &self.cloud.error {
            ui.label(RichText::new(e).color(theme::rgb(0xC42B1C)));
        }
    }

    /// File > Account: signed-in FreedomOffice servers and "Add server".
    fn server_accounts_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("FreedomOffice Server").size(17.0).color(theme::text()));
        ui.add_space(4.0);
        ui.label(RichText::new("Open, save and share workbooks on your own document server.").color(theme::text_dim()));
        ui.add_space(10.0);
        let mut sign_out = None;
        let mut again = None;
        let mut password = None;
        for (i, e) in self.cloud.servers.iter().enumerate() {
            let u = server_user(&e.session);
            ui.horizontal(|ui| {
                initials_badge(ui, &u.initials(), 40.0);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(format!("{} ({})", u.name, u.email)).size(14.5).color(theme::text()));
                    ui.label(RichText::new(&e.session.account.url).size(12.5).color(theme::text_dim()));
                    if let Some(err) = &e.error {
                        ui.label(RichText::new(err).size(12.5).color(error_red()));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new("Sign out").min_size(Vec2::new(90.0, 26.0))).on_hover_text("Signs this PC out on the server too").clicked() {
                        sign_out = Some(i);
                    }
                    if e.error.is_some() && ui.add(egui::Button::new("Sign in again").min_size(Vec2::new(110.0, 26.0))).clicked() {
                        again = Some(i);
                    }
                });
            });
            if e.error.is_none() {
                ui.horizontal(|ui| {
                    ui.add_space(46.0);
                    if ui.add(egui::Button::new("Change password…").min_size(Vec2::new(130.0, 24.0))).clicked() {
                        password = Some(i);
                    }
                    if e.me.as_ref().is_some_and(Me::can_manage) {
                        let url = format!("{}/admin", e.session.account.url);
                        if ui.add(egui::Button::new("Manage organisations…").min_size(Vec2::new(160.0, 24.0))).on_hover_text(format!("Opens {url} in your browser")).clicked() {
                            cloud::open_in_browser(&url);
                        }
                    }
                });
                let orgs: Vec<&str> = e.orgs().iter().map(|o| o.name.as_str()).collect();
                if !orgs.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(46.0);
                        ui.label(RichText::new(format!("Organisations: {}", orgs.join(", "))).size(12.5).color(theme::text_dim()));
                    });
                }
            }
            ui.add_space(8.0);
        }
        if let Some(i) = password {
            self.server_change_password(i);
        }
        if let Some(i) = again {
            let a = &self.cloud.servers[i].session.account;
            self.cloud.add_server = AddServer { open: true, stage: 1, url_input: a.url.clone(), url: a.url.clone(), username: a.username.clone(), ..Default::default() };
        }
        if let Some(i) = sign_out {
            self.server_sign_out(i);
        }
        if self.cloud.add_server.open {
            ui.add_space(4.0);
            self.add_server_ui(ui);
        } else if ui.add(egui::Button::new("Add server").min_size(Vec2::new(120.0, 28.0))).clicked() {
            self.cloud.add_server = AddServer { open: true, ..Default::default() };
        }
    }

    /// Server address, then username and password.
    fn add_server_ui(&mut self, ui: &mut egui::Ui) {
        let mut next = false;
        let mut sign_in = false;
        let mut back = false;
        let mut cancel = false;
        let has_servers = !self.cloud.servers.is_empty();
        let a = &mut self.cloud.add_server;
        egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
            ui.set_max_width(520.0);
            if a.stage == 0 {
                ui.label("Server address:");
                let r = ui.add_enabled(!a.busy, egui::TextEdit::singleline(&mut a.url_input).hint_text("https://files.example.com").desired_width(380.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    next = true;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("Next").min_size(Vec2::new(90.0, 26.0)).fill(theme::accent_light())).clicked() {
                        next = true;
                    }
                    if has_servers && ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            } else {
                ui.label(RichText::new(format!("Sign in to {}", server::host_of(&a.url))).size(15.0).color(theme::text()));
                ui.add_space(6.0);
                egui::Grid::new("server_sign_in").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Username:");
                    ui.add_enabled(!a.busy, egui::TextEdit::singleline(&mut a.username).desired_width(260.0));
                    ui.end_row();
                    ui.label("Password:");
                    let r = ui.add_enabled(!a.busy, egui::TextEdit::singleline(&mut a.password).password(true).desired_width(260.0));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        sign_in = true;
                    }
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = !a.busy && !a.username.trim().is_empty() && !a.password.is_empty();
                    let b = egui::Button::new(RichText::new("Sign in").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(100.0, 28.0));
                    if ui.add_enabled(ok, b).clicked() {
                        sign_in = true;
                    }
                    if ui.add_enabled(!a.busy, egui::Button::new("Back")).clicked() {
                        back = true;
                    }
                    if a.busy {
                        ui.add(egui::Spinner::new());
                        ui.label("Signing in...");
                    }
                });
                ui.add_space(4.0);
                let note = if a.url.starts_with("https://") {
                    if cfg!(windows) { "Your password goes only to this server, encrypted (HTTPS), and isn't stored. This PC keeps a device sign-in, encrypted for your Windows account." } else { "Your password goes only to this server, encrypted (HTTPS), and isn't stored. This computer keeps a device sign-in in a file only your user account can read." }
                } else {
                    "Local test server (http://localhost): the connection isn't encrypted. Your password isn't stored."
                };
                ui.add(egui::Label::new(RichText::new(note).size(12.0).color(theme::text_dim())).wrap());
            }
            if let Some(e) = &a.error {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(e).color(error_red())).wrap());
            }
        });
        if next {
            match server::normalize_url(&a.url_input) {
                Ok(url) => {
                    a.url = url;
                    a.stage = 1;
                    a.error = None;
                }
                Err(e) => a.error = Some(e),
            }
        }
        if back {
            a.stage = 0;
            a.password.clear();
            a.error = None;
        }
        if cancel {
            *a = AddServer::default();
        }
        if sign_in && !a.busy {
            self.server_sign_in();
        }
    }

    // ------------------------------------------------------------------
    // Tab strip: AutoSave, Share, Version History, account
    // ------------------------------------------------------------------

    /// Right end of the ribbon tab row.
    pub fn cloud_tab_strip_ui(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_space(6.0);
            match self.cloud.user.clone() {
                Some(u) => {
                    let r = initials_badge(ui, &u.initials(), 26.0);
                    if r.on_hover_text(format!("{}\n{}", u.name, u.email)).clicked() {
                        self.commit_edit(false);
                        self.backstage = Some(Backstage::Account);
                    }
                }
                None => {
                    if ui.add(egui::Button::new(RichText::new("Sign in").size(12.5)).frame(false)).on_hover_text("Sign in to Microsoft 365").clicked() {
                        self.commit_edit(false);
                        self.backstage = Some(Backstage::Account);
                    }
                }
            }
            // Share (green, like the desktop app)
            let (g, is_icon) = self.icon(SHARE, "");
            let mut job = egui::text::LayoutJob::default();
            if !g.is_empty() {
                let f = if is_icon { theme::icon_font(13.0) } else { theme::ui_font(13.0) };
                job.append(&g, 0.0, egui::TextFormat { font_id: f, color: Color32::WHITE, valign: Align::Center, ..Default::default() });
            }
            job.append("Share", if g.is_empty() { 0.0 } else { 6.0 }, egui::TextFormat { font_id: theme::ui_font(13.0), color: Color32::WHITE, valign: Align::Center, ..Default::default() });
            let galley = ui.painter().layout_job(job);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 24.0, 26.0), Sense::click());
            let fill = if resp.hovered() { share_green().gamma_multiply(0.85) } else { share_green() };
            ui.painter().rect_filled(rect, 4.0, fill);
            ui.painter().galley(Pos2::new(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
            if resp.on_hover_text("Share this workbook with others").clicked() {
                self.commit_edit(false);
                self.cloud_open_share();
            }
            // Others editing with you (live co-editing)
            self.live_avatars_ui(ui);
            // Version History (FreedomOffice server documents)
            if matches!(self.cloud.doc.as_ref().map(|d| &d.provider), Some(Provider::FreedomServer(_))) {
                let (g, is_icon) = self.icon(HISTORY, "🕘");
                let t = RichText::new(g).font(if is_icon { theme::icon_font(14.0) } else { theme::ui_font(14.0) }).color(theme::text());
                if ui.add(egui::Button::new(t).frame(false)).on_hover_text("Version History").clicked() {
                    self.commit_edit(false);
                    self.cloud_open_versions();
                }
            }
            // AutoSave toggle (cloud documents only)
            let live = self.live.is_some();
            let enabled = self.cloud.doc.is_some() && !live;
            let on = (self.cloud.autosave && self.cloud.doc.is_some()) || live;
            let (tr, tresp) = ui.allocate_exact_size(Vec2::new(36.0, 18.0), Sense::click());
            let p = ui.painter();
            let alpha = if enabled { 1.0 } else { 0.45 };
            if on {
                p.rect_filled(tr, 9.0, theme::accent().gamma_multiply(alpha));
                p.circle_filled(Pos2::new(tr.right() - 9.0, tr.center().y), 5.0, theme::window());
            } else {
                p.rect_stroke(tr, 9.0, Stroke::new(1.0, theme::text_dim().gamma_multiply(alpha)), StrokeKind::Inside);
                p.circle_filled(Pos2::new(tr.left() + 9.0, tr.center().y), 5.0, theme::text_dim().gamma_multiply(alpha));
            }
            let tip = match self.cloud.doc.as_ref().map(|d| &d.provider) {
                Some(Provider::FreedomServer(url)) if live => format!("Everyone's changes are saved to {} as they are made. AutoSave stays on while a workbook is edited live.", server::host_of(url)),
                Some(Provider::FreedomServer(url)) => format!("AutoSave saves your changes to {} a few seconds after you stop editing.", server::host_of(url)),
                Some(_) => "AutoSave saves your changes to OneDrive a few seconds after you stop editing.".into(),
                None => "AutoSave works for files in OneDrive, SharePoint and on FreedomOffice servers.".into(),
            };
            if tresp.on_hover_text(tip).clicked() && enabled {
                self.cloud.autosave = !self.cloud.autosave;
            }
            ui.label(RichText::new(if on { "AutoSave On" } else { "AutoSave Off" }).size(12.0).color(if enabled || live { theme::text() } else { theme::text_dim() }));
        });
    }

    pub fn cloud_open_share(&mut self) {
        let mut sh = ShareState { can_edit: true, notify: true, link_anyone: true, ..Default::default() };
        if self.cloud.doc.is_some() && self.cloud.session.is_some() {
            sh.tab = 0;
        }
        sh.link_edit = false;
        self.cloud.share = Some(sh);
    }

    pub fn cloud_open_versions(&mut self) {
        let Some(doc) = self.cloud.doc.clone() else { return };
        let Some(s) = self.doc_server() else {
            self.status = Some(format!("Sign in to {} (File > Account) to see the version history.", cloud::place_name(&doc.provider)));
            return;
        };
        let id = doc.item_id.clone();
        self.cloud.versions = Some(VersionsState { doc, list: None, busy: true, error: None });
        self.cloud_spawn(move || Event::Versions(s.versions(&id)));
    }

    // ------------------------------------------------------------------
    // Dialogs: Share, save conflict, version history
    // ------------------------------------------------------------------

    /// Share, version history or a save conflict is open.
    pub(super) fn cloud_dialog_open(&self) -> bool {
        self.cloud.share.is_some() || self.cloud.versions.is_some() || self.cloud.conflict.is_some() || self.cloud.password.is_some()
    }

    pub fn cloud_dialogs_ui(&mut self, ctx: &egui::Context) {
        self.conflict_ui(ctx);
        self.share_ui(ctx);
        self.versions_ui(ctx);
        self.password_ui(ctx);
    }

    /// "Choose a new password" / "Change password".
    fn password_ui(&mut self, ctx: &egui::Context) {
        let Some(mut f) = self.cloud.password.take() else { return };
        let host = server::host_of(&f.url);
        let mut submit = false;
        let mut close = false;
        let title = if f.forced { "Choose a new password" } else { "Change password" };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_width(420.0);
            let intro = if f.forced {
                format!("You signed in to {host} as {} with a temporary password. Choose your own password to continue.", f.username)
            } else {
                format!("Change the password of {} on {host}. Your other devices will be signed out.", f.username)
            };
            ui.add(egui::Label::new(RichText::new(intro).color(theme::text())).wrap());
            ui.add_space(8.0);
            egui::Grid::new("password_form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(if f.forced { "Temporary password:" } else { "Current password:" });
                let r = ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.old).password(true).desired_width(220.0));
                if !f.focused {
                    f.focused = true;
                    r.request_focus();
                }
                ui.end_row();
                ui.label("New password:");
                ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.new).password(true).desired_width(220.0));
                ui.end_row();
                ui.label("Repeat new password:");
                let r = ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.repeat).password(true).desired_width(220.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(RichText::new("At least 12 characters; not your username and not a common password.").size(12.0).color(theme::text_dim()));
            if let Some(e) = &f.error {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(e).color(error_red())).wrap());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let b = egui::Button::new(RichText::new("Change password").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(140.0, 28.0));
                if ui.add_enabled(!f.busy, b).clicked() {
                    submit = true;
                }
                if ui.add_enabled(!f.busy, egui::Button::new(if f.forced { "Not now" } else { "Cancel" })).clicked() || (!f.busy && ui.input(|i| i.key_pressed(egui::Key::Escape))) {
                    close = true;
                }
                if f.busy {
                    ui.add(egui::Spinner::new());
                    ui.label("Changing...");
                }
            });
        });
        if close {
            // Typed passwords go with the form. "Not now": asked again on the next refusal.
            if let Some(e) = self.cloud.servers.iter().find(|e| e.session.account.url == f.url) {
                e.session.dismiss_password_change();
            }
            if f.forced {
                self.status = Some(format!("Choose a new password (File > Account) to use {host}."));
            }
            return;
        }
        self.cloud.password = Some(f);
        if submit {
            self.password_submit();
        }
    }

    fn conflict_ui(&mut self, ctx: &egui::Context) {
        let Some(c) = self.cloud.conflict.take() else { return };
        let name = self.cloud.doc.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| self.wb.file_name());
        let place = self.cloud.doc.as_ref().map(|d| cloud::place_name(&d.provider)).unwrap_or_else(|| "OneDrive".into());
        let mut keep = true;
        let win = egui::Window::new("FreedomOffice Sheets").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]);
        win.show(ctx, |ui| {
            ui.set_min_width(420.0);
            match &c {
                Conflict::Changed => {
                    ui.label(RichText::new(format!("Someone else saved changes to {name}.")).size(15.0));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Save a copy to keep both versions, or overwrite their changes with yours.").color(theme::text_dim()));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Save a Copy").min_size(Vec2::new(110.0, 26.0)).fill(theme::accent_light())).clicked() {
                            keep = false;
                            self.cloud_upload(UploadKind::Copy, true);
                        }
                        if ui.add(egui::Button::new("Overwrite").min_size(Vec2::new(110.0, 26.0))).clicked() {
                            keep = false;
                            self.cloud_upload(UploadKind::Save, false);
                        }
                        if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(90.0, 26.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                            self.status = Some(format!("Not saved to {place}. Your changes are saved on this PC ({name})."));
                        }
                    });
                }
                Conflict::Exists { conn, folder, name, local } => {
                    let place = cloud::place_name(&conn.provider());
                    ui.label(RichText::new(format!("A file named '{name}' already exists in this location.")).size(15.0));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Do you want to replace it?").color(theme::text_dim()));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Replace").min_size(Vec2::new(100.0, 26.0)).fill(theme::accent_light())).clicked() {
                            keep = false;
                            self.start_new_upload(conn.clone(), folder.clone(), name.clone(), local.clone(), true);
                        }
                        if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(90.0, 26.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                            self.status = Some(format!("Not saved to {place}. Your changes are saved on this PC."));
                        }
                    });
                }
            }
        });
        if keep {
            self.cloud.conflict = Some(c);
        }
    }

    fn share_ui(&mut self, ctx: &egui::Context) {
        if matches!(self.cloud.doc.as_ref().map(|d| &d.provider), Some(Provider::FreedomServer(_))) {
            self.server_share_ui(ctx);
            return;
        }
        let Some(mut sh) = self.cloud.share.take() else { return };
        let mut open = true;
        let doc = self.cloud.doc.clone();
        let title = match &doc {
            Some(d) => format!("Share '{}'", d.name),
            None => "Share".to_string(),
        };
        egui::Window::new(title).open(&mut open).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_width(460.0);
            let Some(doc) = doc else {
                ui.label(RichText::new("To share this workbook, upload it to OneDrive first.").size(14.0));
                ui.add_space(4.0);
                ui.label(RichText::new("People you share with can then open the same file and see your saved changes.").color(theme::text_dim()));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let b = egui::Button::new(RichText::new("Upload to OneDrive").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(160.0, 30.0));
                    if ui.add(b).clicked() {
                        self.cloud.save_place = Place::MyFiles;
                        self.cloud.browser.root = None;
                        self.cloud.save_name = self.default_cloud_name();
                        self.backstage = Some(Backstage::SaveAs);
                        sh.tab = 255;
                    }
                    let servers: Vec<(usize, String)> = self.cloud.servers.iter().enumerate().map(|(i, e)| (i, e.session.host())).collect();
                    for (i, host) in servers {
                        let b = egui::Button::new(RichText::new(format!("Upload to {host}")).color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(160.0, 30.0));
                        if ui.add(b).clicked() {
                            self.cloud.save_place = Place::Server(i, ServerView::MyFiles);
                            self.cloud.browser.root = None;
                            self.cloud.save_name = self.default_cloud_name();
                            self.backstage = Some(Backstage::SaveAs);
                            sh.tab = 255;
                        }
                    }
                });
                return;
            };
            let session = self.cloud.session.clone();
            if session.is_none() {
                ui.label(RichText::new("Sign in to share this workbook.").color(theme::text_dim()));
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                for (k, label) in [(0u8, "Invite people"), (1, "Copy link"), (2, "Manage access")] {
                    if ui.selectable_label(sh.tab == k, RichText::new(label).size(13.5)).clicked() && sh.tab != k {
                        sh.tab = k;
                        sh.info = None;
                        sh.error = None;
                    }
                }
            });
            ui.separator();
            ui.add_space(4.0);
            let path = doc.item_path();
            match sh.tab {
                0 => {
                    ui.label("To:");
                    ui.add(egui::TextEdit::singleline(&mut sh.emails).hint_text("Add names or email addresses, separated by ;").desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("share_invite_role").selected_text(if sh.can_edit { "Can edit" } else { "Can view" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut sh.can_edit, true, "Can edit");
                            ui.selectable_value(&mut sh.can_edit, false, "Can view");
                        });
                        ui.checkbox(&mut sh.notify, "Notify people");
                    });
                    ui.add_space(4.0);
                    ui.add(egui::TextEdit::multiline(&mut sh.message).hint_text("Add a message (optional)").desired_rows(3).desired_width(f32::INFINITY));
                    ui.add_space(6.0);
                    let emails: Vec<String> = sh.emails.split([';', ',', ' ', '\n']).map(str::trim).filter(|e| !e.is_empty()).map(str::to_string).collect();
                    let valid = !emails.is_empty() && emails.iter().all(|e| e.contains('@'));
                    ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 30.0), egui::Layout::right_to_left(Align::Center), |ui| {
                        let b = egui::Button::new(RichText::new("Send").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(90.0, 28.0));
                        if ui.add_enabled(valid && !sh.busy && session.is_some(), b).clicked() {
                            if let Some(s) = session.clone() {
                                sh.busy = true;
                                sh.info = None;
                                sh.error = None;
                                let (edit, msg, notify, n) = (sh.can_edit, sh.message.clone(), sh.notify, emails.len());
                                let p = path.clone();
                                self.cloud_spawn(move || Event::Invited(s.invite(&p, &emails, edit, &msg, notify).map(|_| n)));
                            }
                        }
                    });
                }
                1 => {
                    ui.label("Who can use the link:");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("share_link_scope")
                            .width(240.0)
                            .selected_text(if sh.link_anyone { "Anyone with the link" } else { "People in your organization" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut sh.link_anyone, true, "Anyone with the link");
                                ui.selectable_value(&mut sh.link_anyone, false, "People in your organization");
                            });
                        egui::ComboBox::from_id_salt("share_link_role").selected_text(if sh.link_edit { "Can edit" } else { "Can view" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut sh.link_edit, true, "Can edit");
                            ui.selectable_value(&mut sh.link_edit, false, "Can view");
                        });
                    });
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 30.0), egui::Layout::right_to_left(Align::Center), |ui| {
                        let b = egui::Button::new(RichText::new("Copy link").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(100.0, 28.0));
                        if ui.add_enabled(!sh.busy && session.is_some(), b).clicked() {
                            if let Some(s) = session.clone() {
                                sh.busy = true;
                                sh.info = None;
                                sh.error = None;
                                let (anyone, edit) = (sh.link_anyone, sh.link_edit);
                                let p = path.clone();
                                self.cloud_spawn(move || Event::Link(s.create_link(&p, anyone, edit)));
                            }
                        }
                    });
                    if let Some(l) = &sh.link {
                        ui.add_space(6.0);
                        let mut t = l.clone();
                        ui.add(egui::TextEdit::singleline(&mut t).desired_width(f32::INFINITY));
                    }
                }
                _ => {
                    if sh.perms.is_none() && !sh.busy {
                        if let Some(s) = session.clone() {
                            sh.busy = true;
                            let p = path.clone();
                            self.cloud_spawn(move || Event::Permissions(s.permissions(&p)));
                        }
                    }
                    if let Some(id) = permissions_list_ui(ui, &sh) {
                        if let Some(s) = session.clone() {
                            sh.busy = true;
                            sh.error = None;
                            let p = path.clone();
                            self.cloud_spawn(move || {
                                let r = s.delete_permission(&p, &id).and_then(|_| s.permissions(&p));
                                Event::Permissions(r)
                            });
                        }
                    }
                }
            }
            share_status_ui(ui, &sh);
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(!doc.web_url.is_empty(), egui::Button::new("Open in browser")).clicked() {
                    cloud::open_in_browser(&doc.web_url);
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        sh.tab = 255;
                    }
                });
            });
        });
        if open && sh.tab != 255 {
            self.cloud.share = Some(sh);
        }
    }

    /// Share dialog for documents on a FreedomOffice server: invite server users, links with
    /// expiry and password, manage access.
    fn server_share_ui(&mut self, ctx: &egui::Context) {
        let Some(mut sh) = self.cloud.share.take() else { return };
        let Some(doc) = self.cloud.doc.clone() else { return };
        let session = self.doc_server();
        let host = cloud::place_name(&doc.provider);
        // The organisations this account belongs to (to share with all their members).
        let orgs: Vec<OrgInfo> = match &doc.provider {
            Provider::FreedomServer(url) => self.server_index(url).map(|i| self.cloud.servers[i].orgs().to_vec()).unwrap_or_default(),
            Provider::Microsoft => Vec::new(),
        };
        let mut open = true;
        let mut versions = false;
        egui::Window::new(format!("Share '{}'", doc.name)).open(&mut open).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_width(480.0);
            if session.is_none() {
                ui.label(RichText::new(format!("Sign in to {host} (File > Account) to share this workbook.")).color(theme::text_dim()));
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                for (k, label) in [(0u8, "Invite people"), (1, "Copy link"), (2, "Manage access")] {
                    if ui.selectable_label(sh.tab == k, RichText::new(label).size(13.5)).clicked() && sh.tab != k {
                        sh.tab = k;
                        sh.focused = false;
                        sh.info = None;
                        sh.error = None;
                    }
                }
            });
            ui.separator();
            ui.add_space(4.0);
            let id = doc.item_id.clone();
            match sh.tab {
                0 => {
                    ui.label(format!("People with an account on {host}:"));
                    let r = ui.add(egui::TextEdit::singleline(&mut sh.user_query).hint_text("Search by name or username").desired_width(f32::INFINITY));
                    ui.label(RichText::new("Finds people who share an organisation with you. Anyone else: type their exact username.").size(12.0).color(theme::text_dim()));
                    if !sh.focused {
                        sh.focused = true;
                        r.request_focus();
                    }
                    if sh.user_query != sh.searched {
                        sh.searched = sh.user_query.clone();
                        sh.user_seq += 1;
                        if sh.user_query.trim().is_empty() {
                            sh.user_results.clear();
                        } else if let Some(s) = session.clone() {
                            let (q, seq) = (sh.user_query.clone(), sh.user_seq);
                            self.cloud_spawn(move || Event::Users(seq, s.search_users(&q)));
                        }
                    }
                    let mut add = None;
                    if !sh.user_results.is_empty() {
                        egui::Frame::group(ui.style()).inner_margin(4.0).show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                                for (u, name) in &sh.user_results {
                                    let chosen = sh.invite.iter().any(|(x, _)| x == u);
                                    let t = RichText::new(format!("{name}  ({u})")).color(if chosen { theme::text_dim() } else { theme::text() });
                                    if ui.add_enabled(!chosen, egui::Button::new(t).frame(false)).clicked() {
                                        add = Some((u.clone(), name.clone()));
                                    }
                                }
                            });
                        });
                    } else if !sh.user_query.trim().is_empty() {
                        ui.label(RichText::new("No matching people in your organisations, and no one with exactly that username.").size(12.0).color(theme::text_dim()));
                    }
                    if let Some(p) = add {
                        sh.invite.push(p);
                    }
                    if !sh.invite.is_empty() {
                        ui.add_space(4.0);
                        let mut remove = None;
                        ui.horizontal_wrapped(|ui| {
                            for (i, (_, name)) in sh.invite.iter().enumerate() {
                                if ui.button(format!("{name}  ✕")).on_hover_text("Remove").clicked() {
                                    remove = Some(i);
                                }
                            }
                        });
                        if let Some(i) = remove {
                            sh.invite.remove(i);
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("server_invite_role").selected_text(if sh.can_edit { "Can edit" } else { "Can view" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut sh.can_edit, true, "Can edit");
                            ui.selectable_value(&mut sh.can_edit, false, "Can view");
                        });
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            let b = egui::Button::new(RichText::new("Share").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(90.0, 28.0));
                            if ui.add_enabled(!sh.invite.is_empty() && !sh.busy && session.is_some(), b).clicked() {
                                if let Some(s) = session.clone() {
                                    sh.busy = true;
                                    sh.info = None;
                                    sh.error = None;
                                    let (edit, people, id) = (sh.can_edit, sh.invite.clone(), id.clone());
                                    self.cloud_spawn(move || {
                                        let r = people.iter().try_for_each(|(u, _)| s.grant(&id, u, edit)).map(|_| people.len());
                                        Event::Invited(r)
                                    });
                                }
                            }
                        });
                    });
                    ui.label(RichText::new("They'll find it under Shared with me.").size(12.0).color(theme::text_dim()));
                    if !orgs.is_empty() {
                        ui.add_space(6.0);
                        ui.separator();
                        ui.label("Everyone in an organisation:");
                        sh.org_pick = sh.org_pick.min(orgs.len() - 1);
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("server_share_org").width(190.0).selected_text(orgs[sh.org_pick].name.clone()).show_ui(ui, |ui| {
                                for (k, o) in orgs.iter().enumerate() {
                                    ui.selectable_value(&mut sh.org_pick, k, o.name.clone());
                                }
                            });
                            egui::ComboBox::from_id_salt("server_share_org_role").selected_text(if sh.org_edit { "Can edit" } else { "Can view" }).show_ui(ui, |ui| {
                                ui.selectable_value(&mut sh.org_edit, true, "Can edit");
                                ui.selectable_value(&mut sh.org_edit, false, "Can view");
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                let b = egui::Button::new(RichText::new("Share").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(90.0, 28.0));
                                if ui.add_enabled(!sh.busy && session.is_some(), b).on_hover_text(format!("Everyone in {} gets access, also people who join later.", orgs[sh.org_pick].name)).clicked() {
                                    if let Some(s) = session.clone() {
                                        sh.busy = true;
                                        sh.info = None;
                                        sh.error = None;
                                        let (edit, org, id) = (sh.org_edit, orgs[sh.org_pick].clone(), id.clone());
                                        self.cloud_spawn(move || Event::OrgShared(s.grant_org(&id, &org.id, edit).map(|_| org.name)));
                                    }
                                }
                            });
                        });
                    }
                }
                1 => {
                    ui.label("Anyone with the link:");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("server_link_role").selected_text(if sh.link_edit { "Can edit" } else { "Can view" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut sh.link_edit, true, "Can edit");
                            ui.selectable_value(&mut sh.link_edit, false, "Can view");
                        });
                        let label = LINK_DAYS.iter().find(|(d, _)| *d == sh.link_days).map(|(_, l)| *l).unwrap_or("Never expires");
                        egui::ComboBox::from_id_salt("server_link_days").width(170.0).selected_text(label).show_ui(ui, |ui| {
                            for (d, l) in LINK_DAYS {
                                ui.selectable_value(&mut sh.link_days, *d, *l);
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Password (optional):");
                        ui.add(egui::TextEdit::singleline(&mut sh.link_password).password(true).desired_width(200.0));
                    });
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 30.0), egui::Layout::right_to_left(Align::Center), |ui| {
                        let b = egui::Button::new(RichText::new("Copy link").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(100.0, 28.0));
                        if ui.add_enabled(!sh.busy && session.is_some(), b).clicked() {
                            if let Some(s) = session.clone() {
                                sh.busy = true;
                                sh.info = None;
                                sh.error = None;
                                let (edit, days, pw, id) = (sh.link_edit, sh.link_days, sh.link_password.clone(), id.clone());
                                self.cloud_spawn(move || Event::Link(s.create_link(&id, edit, Some(days), &pw)));
                            }
                        }
                    });
                    if let Some(l) = &sh.link {
                        ui.add_space(6.0);
                        let mut t = l.clone();
                        ui.add(egui::TextEdit::singleline(&mut t).desired_width(f32::INFINITY));
                        ui.label(RichText::new("The link is shown only once; copy it now. Remove it under Manage access.").size(12.0).color(theme::text_dim()));
                    }
                }
                _ => {
                    if sh.perms.is_none() && !sh.busy {
                        if let Some(s) = session.clone() {
                            sh.busy = true;
                            let id = id.clone();
                            self.cloud_spawn(move || Event::Permissions(s.permissions(&id)));
                        }
                    }
                    if let Some(pid) = permissions_list_ui(ui, &sh) {
                        if let Some(s) = session.clone() {
                            sh.busy = true;
                            sh.error = None;
                            let id = id.clone();
                            self.cloud_spawn(move || Event::Permissions(s.remove_permission(&id, &pid).and_then(|_| s.permissions(&id))));
                        }
                    }
                }
            }
            share_status_ui(ui, &sh);
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(session.is_some(), egui::Button::new("Version History")).clicked() {
                    versions = true;
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        sh.tab = 255;
                    }
                });
            });
        });
        if open && sh.tab != 255 && !versions {
            self.cloud.share = Some(sh);
        }
        if versions {
            self.cloud_open_versions();
        }
    }

    fn versions_ui(&mut self, ctx: &egui::Context) {
        let Some(mut v) = self.cloud.versions.take() else { return };
        let mut open = true;
        let mut close = false;
        let session = self.doc_server();
        let dirty = self.dirty;
        egui::Window::new(format!("Version History - {}", v.doc.name)).open(&mut open).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new("Restore makes an older version the current one; the newer versions stay in the history.").size(12.5).color(theme::text_dim()));
            ui.add_space(6.0);
            let mut restore = None;
            let mut copy = None;
            match &v.list {
                None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label("Loading...");
                    });
                }
                Some(list) if list.is_empty() => {
                    ui.label(RichText::new("No versions.").color(theme::text_dim()));
                }
                Some(list) => {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        egui::Grid::new("server_versions").num_columns(5).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
                            for h in ["Version", "Modified", "Modified by", "Size", ""] {
                                ui.label(RichText::new(h).size(12.0).color(theme::text_dim()));
                            }
                            ui.end_row();
                            for x in list {
                                ui.label(if x.current { format!("{} (current)", x.version) } else { x.version.to_string() });
                                ui.label(graph::short_date(&x.modified));
                                ui.label(&x.modified_by);
                                ui.label(size_label(x.size as usize));
                                ui.horizontal(|ui| {
                                    if ui.add_enabled(!v.busy, egui::Button::new("Open a copy")).clicked() {
                                        copy = Some(x.version);
                                    }
                                    if !x.current {
                                        let b = ui.add_enabled(!v.busy && !dirty, egui::Button::new("Restore"));
                                        if b.on_disabled_hover_text("Save or undo your changes first").clicked() {
                                            restore = Some(x.version);
                                        }
                                    }
                                });
                                ui.end_row();
                            }
                        });
                    });
                }
            }
            if let Some(e) = &v.error {
                ui.label(RichText::new(e).color(error_red()));
            }
            if v.busy && v.list.is_some() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("Working...");
                });
            }
            if let (Some(n), Some(s)) = (restore, session.clone()) {
                v.busy = true;
                v.error = None;
                let id = v.doc.item_id.clone();
                self.cloud_spawn(move || Event::VersionRestored(s.restore_version(&id, n)));
            }
            if let (Some(n), Some(s)) = (copy, session.clone()) {
                let dest = v.doc.local.parent().map(|d| d.join("versions").join(server::version_copy_name(&v.doc.name, n)));
                if let Some(dest) = dest {
                    v.busy = true;
                    v.error = None;
                    let id = v.doc.item_id.clone();
                    self.cloud_spawn(move || Event::VersionCopy(s.download_version(&id, n, &dest).map(|_| dest)));
                }
            }
            ui.add_space(6.0);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        });
        if open && !close {
            self.cloud.versions = Some(v);
        }
    }
}

/// After a new file was uploaded to a FreedomOffice server: downloads it as Open does, for the
/// live session it can join (worker thread).
fn reopen_new(conn: &Conn, root: Option<&std::path::Path>, result: &Result<DriveItem, UploadError>) -> Option<Result<CloudDoc, String>> {
    let (Conn::Server(s), Ok(item), Some(root)) = (conn, result, root) else { return None };
    Some(s.open(item, root))
}

/// Manage access rows; returns the id of a row whose Remove button was clicked.
fn permissions_list_ui(ui: &mut egui::Ui, sh: &ShareState) -> Option<String> {
    let mut remove: Option<String> = None;
    match &sh.perms {
        None if sh.busy => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Loading...");
            });
        }
        None => {}
        Some(list) if list.is_empty() => {
            ui.label(RichText::new("Only you have access.").color(theme::text_dim()));
        }
        Some(list) => {
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                for p in list {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(300.0);
                            ui.label(RichText::new(&p.who).color(theme::text()));
                            if !p.detail.is_empty() && p.detail != p.who {
                                ui.label(RichText::new(&p.detail).size(11.5).color(theme::text_dim()));
                            }
                        });
                        ui.label(RichText::new(p.role_label()).size(12.5).color(theme::text_dim()));
                        if p.removable() {
                            if ui.add_enabled(!sh.busy, egui::Button::new("Remove")).clicked() {
                                remove = Some(p.id.clone());
                            }
                        } else if p.inherited {
                            ui.label(RichText::new("(from folder)").size(11.5).color(theme::text_dim()));
                        }
                    });
                    ui.add_space(2.0);
                }
            });
        }
    }
    remove
}

fn share_status_ui(ui: &mut egui::Ui, sh: &ShareState) {
    if sh.busy && sh.tab != 2 {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new());
            ui.label("Working...");
        });
    }
    if let Some(i) = &sh.info {
        ui.label(RichText::new(i).color(theme::accent_dark()));
    }
    if let Some(e) = &sh.error {
        ui.label(RichText::new(e).color(theme::rgb(0xC42B1C)));
    }
}

fn size_label(n: usize) -> String {
    if n >= 1024 * 1024 { format!("{:.1} MB", n as f64 / 1048576.0) } else { format!("{} KB", n.div_ceil(1024)) }
}

fn initials_badge(ui: &mut egui::Ui, initials: &str, d: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d), Sense::click());
    let fill = if resp.hovered() { theme::accent_dark() } else { theme::accent() };
    ui.painter().circle_filled(rect.center(), d / 2.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, initials, theme::ui_bold(d * 0.4), theme::window());
    resp
}

#[cfg(test)]
impl App {
    /// Signed in to a server without touching the saved accounts (tests).
    pub(super) fn cloud_for_test(&mut self, session: Arc<ServerSession>) {
        self.cloud.started = true;
        if self.cloud.cache_override.is_none() {
            self.cloud.cache_override = Some(std::env::temp_dir().join(format!("fo-sheets-cache-{}", std::process::id())));
        }
        let me = session.me_info().ok();
        self.cloud.servers.push(ServerEntry { session, error: None, me });
    }

    /// No saved accounts or sign-ins are read from this PC (tests).
    pub(super) fn no_cloud_for_test(&mut self) {
        self.cloud.started = true;
        self.cloud.cache_override = Some(std::env::temp_dir().join(format!("fo-sheets-cache-{}", std::process::id())));
    }

    /// As if the cloud browser had downloaded `doc` (tests).
    pub(super) fn cloud_opened_for_test(&mut self, doc: CloudDoc) {
        self.cloud_event(Event::Opened(Ok(doc)));
    }
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
