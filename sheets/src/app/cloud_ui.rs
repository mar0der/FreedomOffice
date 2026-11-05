//! Microsoft 365 in the UI: Account page, OneDrive / SharePoint places in Open and Save As,
//! saving back with conflict handling, AutoSave, and the Share button and dialog.
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
use crate::cloud::{self, graph, CloudDoc, DriveItem, Kind, Permission, Session, Target, UploadError, User};

const SHARE: char = '\u{E72D}';
const CLOUD: char = '\u{E753}';
const PEOPLE: char = '\u{E716}';
const GLOBE: char = '\u{E774}';
const CLOCK: char = '\u{E823}';
const PC: char = '\u{E7F8}';

const ENTRA_URL: &str = "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade";

/// The steps shown on the Account page when no app registration is configured.
const SETUP_STEPS: &[&str] = &[
    "Sign in to the Microsoft Entra admin center (entra.microsoft.com), open App registrations and select New registration.",
    "Name it, for example \"FreedomOffice Sheets\". Under Supported account types choose \"Accounts in any organizational directory and personal Microsoft accounts\". Under Redirect URI choose \"Public client/native (mobile & desktop)\" and enter http://localhost. Select Register.",
    "Open Authentication, set \"Allow public client flows\" to Yes and select Save.",
    "Open API permissions > Add a permission > Microsoft Graph > Delegated permissions and add openid, profile, offline_access, User.Read, Files.ReadWrite.All and Sites.Read.All. If your organization requires it, ask an admin to grant consent.",
    "Copy the Application (client) ID from the Overview page and paste it below.",
];

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
}

impl Place {
    fn target(self) -> Option<Target> {
        match self {
            Place::Local => None,
            Place::MyFiles => Some(Target::MyFiles),
            Place::Recent => Some(Target::Recent),
            Place::Shared => Some(Target::Shared),
            Place::Sites => Some(Target::Sites),
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
    seq: u64,
}

enum UploadKind {
    /// Save over the open document (with its eTag unless overwriting).
    Save,
    /// "Save a Copy" next to the open document after a conflict.
    Copy,
    /// Save As into a OneDrive / SharePoint folder.
    New { folder: String, name: String },
}

enum Event {
    Session { result: Result<Option<(Arc<Session>, User)>, String>, interactive: bool },
    Listing(u64, Result<Vec<DriveItem>, String>),
    Opened(Result<CloudDoc, String>),
    Uploaded { kind: UploadKind, local: PathBuf, result: Result<DriveItem, UploadError> },
    Invited(Result<usize, String>),
    Link(Result<String, String>),
    Permissions(Result<Vec<Permission>, String>),
}

pub enum Conflict {
    /// HTTP 412 on save.
    Changed,
    /// Save As target name exists.
    Exists { folder: String, name: String, local: PathBuf },
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
    /// The open document lives in OneDrive / SharePoint.
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
        }
    }
}

fn share_green() -> Color32 {
    if theme::is_dark() { theme::rgb(0x37A660) } else { theme::rgb(0x217346) }
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
            self.cloud_demo();
        }
        while let Ok(ev) = self.cloud.rx.try_recv() {
            self.cloud_event(ev);
        }
        // AutoSave: upload a few seconds after the last edit.
        let now = ctx.input(|i| i.time);
        if self.edits != self.cloud.last_edits {
            self.cloud.last_edits = self.edits;
            self.cloud.last_edit_time = now;
        }
        if self.cloud.autosave && self.cloud.doc.is_some() && self.dirty {
            let idle = now - self.cloud.last_edit_time;
            if idle >= 5.0 && self.edit.is_none() && !self.cloud.uploading && self.dialog.is_none() && self.cloud.conflict.is_none() {
                self.save();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
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
            Event::Uploaded { kind, local, result } => self.cloud_uploaded(kind, local, result),
            Event::Invited(result) => {
                if let Some(sh) = &mut self.cloud.share {
                    sh.busy = false;
                    match result {
                        Ok(n) => {
                            sh.info = Some(if n == 1 { "Shared with 1 person.".into() } else { format!("Shared with {n} people.") });
                            sh.error = None;
                            sh.emails.clear();
                            sh.message.clear();
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
        }
    }

    fn cloud_uploaded(&mut self, kind: UploadKind, local: PathBuf, result: Result<DriveItem, UploadError>) {
        self.cloud.uploading = false;
        match (kind, result) {
            (UploadKind::Save, Ok(item)) => {
                if let Some(doc) = &mut self.cloud.doc {
                    doc.update_from(&item);
                }
                self.cloud.upload_failed = false;
                self.status = Some("Saved to OneDrive.".into());
            }
            (UploadKind::Copy | UploadKind::New { .. }, Ok(item)) => {
                let mut doc = CloudDoc::from_item(&item, local);
                if doc.drive_id.is_empty() {
                    doc.drive_id = self.cloud.doc.as_ref().map(|d| d.drive_id.clone()).unwrap_or_default();
                }
                self.status = Some(format!("Saved to OneDrive as {}.", doc.name));
                self.cloud.doc = Some(doc);
                self.cloud.upload_failed = false;
            }
            (UploadKind::Save, Err(UploadError::Conflict)) => {
                self.cloud.upload_failed = true;
                self.cloud.conflict = Some(Conflict::Changed);
            }
            (UploadKind::New { folder, name }, Err(UploadError::Exists)) => {
                self.cloud.upload_failed = true;
                self.cloud.conflict = Some(Conflict::Exists { folder, name, local });
            }
            (_, Err(e)) => {
                self.cloud.upload_failed = true;
                self.status = Some(format!("Couldn't save to OneDrive: {e} Your changes are saved on this PC."));
                if !self.cloud.autosave {
                    self.dialog = Some(Dialog::Message {
                        title: "Save".into(),
                        text: format!("Couldn't upload to OneDrive.\n\n{e}\n\nYour changes are saved on this PC in\n{}", local.display()),
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

    // ------------------------------------------------------------------
    // Saving
    // ------------------------------------------------------------------

    /// Ctrl+S on a cloud document: write the local copy, then upload in the background.
    pub fn cloud_save(&mut self) -> bool {
        let Some(doc) = self.cloud.doc.clone() else { return false };
        let native = self.wb.path.as_ref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| e == "xlsx" || e == "xlsm");
        if !native {
            // .xls / .csv / .ods can't be written back: save a workbook copy instead.
            let stem = doc.name.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(doc.name.clone());
            self.cloud.save_name = format!("{stem}.xlsx");
            self.cloud.save_place = Place::MyFiles;
            if let Some(parent) = &doc.parent {
                self.cloud.browser = Browser { root: Some(Place::MyFiles), ..Default::default() };
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
        let Some(session) = self.cloud.session.clone() else {
            if self.cloud.restoring {
                self.cloud.upload_again = true;
            } else {
                self.cloud.upload_failed = true;
                self.status = Some("Saved on this PC. Sign in (File > Account) to save to OneDrive.".into());
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
        self.status = Some(format!("Saving to OneDrive ({})...", size_label(data.len())));
        self.cloud_spawn(move || {
            let result = match &kind {
                UploadKind::Save => session.upload(&doc.item_path(), check.then_some(doc.etag.as_str()), &data),
                UploadKind::Copy => match &doc.parent {
                    Some(parent) => session.upload_new(parent, &graph::copy_name(&doc.name), &data, "rename"),
                    None => Err(UploadError::Failed("The file's folder is unknown.".into())),
                },
                UploadKind::New { folder, name } => session.upload_new(folder, name, &data, if check { "fail" } else { "replace" }),
            };
            Event::Uploaded { kind, local: doc.local.clone(), result: result.map(|i| with_drive(&session, i)) }
        });
    }

    /// Save As into the folder shown in the cloud browser.
    fn cloud_save_as(&mut self, folder: String, name: String, replace: bool) {
        self.commit_edit(false);
        let Some(session) = self.cloud.session.clone() else { return };
        let mut name = name.trim().to_string();
        if !name.to_lowercase().ends_with(".xlsx") && !name.to_lowercase().ends_with(".xlsm") {
            name.push_str(".xlsx");
        }
        let Some(dir) = cloud::cache_dir().map(|d| d.join(format!("new-{}", cloud::auth::now_secs()))) else { return };
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
        self.start_new_upload(session, folder, name, local, replace);
    }

    fn start_new_upload(&mut self, session: Arc<Session>, folder: String, name: String, local: PathBuf, replace: bool) {
        let data = match std::fs::read(&local) {
            Ok(d) => d,
            Err(e) => {
                self.status = Some(format!("Couldn't read {}: {e}", local.display()));
                return;
            }
        };
        self.cloud.uploading = true;
        self.status = Some(format!("Saving {name} to OneDrive..."));
        self.cloud_spawn(move || {
            let result = session.upload_new(&folder, &name, &data, if replace { "replace" } else { "fail" });
            Event::Uploaded { kind: UploadKind::New { folder, name }, local, result: result.map(|i| with_drive(&session, i)) }
        });
    }

    /// Title bar suffix like "Saved to OneDrive".
    pub fn cloud_title_suffix(&self) -> &'static str {
        match &self.cloud.doc {
            None => "",
            Some(_) if self.cloud.uploading => " - Saving...",
            Some(_) if self.cloud.upload_failed => " - Not saved to OneDrive",
            Some(_) if !self.dirty => " - Saved to OneDrive",
            Some(_) => "",
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
        let seq = b.seq;
        let Some(session) = self.cloud.session.clone() else { return };
        self.cloud_spawn(move || Event::Listing(seq, session.list(&target)));
    }

    fn cloud_open_item(&mut self, item: DriveItem) {
        let (Some(session), Some(root)) = (self.cloud.session.clone(), cloud::cache_dir()) else { return };
        self.cloud.opening = Some(item.name.clone());
        self.status = Some(format!("Downloading {}...", item.name));
        self.cloud_spawn(move || Event::Opened(session.open(&item, &root)));
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
        let current = if save_as { self.cloud.save_place } else { self.cloud.open_place };
        let mut picked = None;
        ui.vertical(|ui| {
            ui.set_width(240.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            for (place, code, fallback, label) in places {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(230.0, 40.0), Sense::click());
                if place == current {
                    ui.painter().rect_filled(rect, 3.0, theme::accent_light());
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, theme::hover());
                }
                let (g, is_icon) = self.icon(code, fallback);
                let font = if is_icon { theme::icon_font(17.0) } else { theme::ui_font(16.0) };
                ui.painter().text(Pos2::new(rect.left() + 20.0, rect.center().y), Align2::CENTER_CENTER, g, font, theme::accent_dark());
                ui.painter().text(Pos2::new(rect.left() + 42.0, rect.center().y), Align2::LEFT_CENTER, label, theme::ui_font(13.5), theme::text());
                if resp.clicked() {
                    picked = Some(place);
                }
            }
        });
        if let Some(p) = picked {
            if save_as { self.cloud.save_place = p } else { self.cloud.open_place = p }
            if p != current {
                self.cloud.browser.root = None;
            }
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
        if self.cloud.session.is_none() {
            self.signed_out_ui(ui);
            return;
        }
        if self.cloud.browser.root != Some(place) {
            let root = place.target().unwrap_or(Target::MyFiles);
            let label = match place {
                Place::Recent => "Recent",
                Place::Shared => "Shared with me",
                Place::Sites => "Sites",
                _ => "My files",
            };
            self.cloud.browser = Browser { root: Some(place), seq: self.cloud.browser.seq, ..Default::default() };
            self.cloud_navigate(root, label.into(), true);
        }
        // Breadcrumb
        let mut go: Option<usize> = None;
        let mut refresh = false;
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
        });
        if let Some(i) = go {
            self.cloud.browser.crumbs.truncate(i + 1);
            refresh = true;
        }
        if refresh {
            if let Some((_, t)) = self.cloud.browser.crumbs.last().cloned() {
                self.cloud_navigate(t, String::new(), false);
            }
        }
        ui.add_space(8.0);
        let folder = self.cloud.browser.crumbs.last().and_then(|(_, t)| t.folder_path());
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
                    let (f, n) = (folder.clone().unwrap_or_default(), self.cloud.save_name.clone());
                    self.cloud_save_as(f, n, false);
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
            match it.target() {
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
                return;
            }
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
                    ui.label(
                        RichText::new("You stay signed in on this PC; the sign-in is stored encrypted for your Windows account.").size(12.0).color(theme::text_dim()),
                    );
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

    // ------------------------------------------------------------------
    // Tab strip: AutoSave, Share, account
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
            // AutoSave toggle (cloud documents only)
            let enabled = self.cloud.doc.is_some();
            let on = self.cloud.autosave && enabled;
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
            let tip = if enabled { "AutoSave saves your changes to OneDrive a few seconds after you stop editing." } else { "AutoSave works for files in OneDrive and SharePoint." };
            if tresp.on_hover_text(tip).clicked() && enabled {
                self.cloud.autosave = !self.cloud.autosave;
            }
            ui.label(RichText::new(if on { "AutoSave On" } else { "AutoSave Off" }).size(12.0).color(if enabled { theme::text() } else { theme::text_dim() }));
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

    // ------------------------------------------------------------------
    // Dialogs: Share, save conflict
    // ------------------------------------------------------------------

    pub fn cloud_dialogs_ui(&mut self, ctx: &egui::Context) {
        self.conflict_ui(ctx);
        self.share_ui(ctx);
    }

    fn conflict_ui(&mut self, ctx: &egui::Context) {
        let Some(c) = self.cloud.conflict.take() else { return };
        let name = self.cloud.doc.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| self.wb.file_name());
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
                            self.status = Some(format!("Not saved to OneDrive. Your changes are saved on this PC ({name})."));
                        }
                    });
                }
                Conflict::Exists { folder, name, local } => {
                    ui.label(RichText::new(format!("A file named '{name}' already exists in this location.")).size(15.0));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Do you want to replace it?").color(theme::text_dim()));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Replace").min_size(Vec2::new(100.0, 26.0)).fill(theme::accent_light())).clicked() {
                            keep = false;
                            if let Some(s) = self.cloud.session.clone() {
                                self.start_new_upload(s, folder.clone(), name.clone(), local.clone(), true);
                            }
                        }
                        if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(90.0, 26.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                            self.status = Some("Not saved to OneDrive. Your changes are saved on this PC.".into());
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
                let b = egui::Button::new(RichText::new("Upload to OneDrive").color(Color32::WHITE)).fill(share_green()).min_size(Vec2::new(160.0, 30.0));
                if ui.add(b).clicked() {
                    self.cloud.save_place = Place::MyFiles;
                    self.cloud.browser.root = None;
                    self.cloud.save_name = self.default_cloud_name();
                    self.backstage = Some(Backstage::SaveAs);
                    sh.tab = 255;
                }
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
                    if let (Some(id), Some(s)) = (remove, session.clone()) {
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
}

/// Fills in the drive id when an upload answer omits it.
fn with_drive(session: &Session, item: DriveItem) -> DriveItem {
    if !item.drive_id.is_empty() || item.id.is_empty() {
        return item;
    }
    session.item(&format!("/me/drive/items/{}", item.id)).unwrap_or(item)
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
