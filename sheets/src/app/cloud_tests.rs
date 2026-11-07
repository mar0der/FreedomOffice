//! Organisations, sharing with an organisation and new passwords in the app, against a real
//! server on 127.0.0.1 (throwaway data and accounts; nothing on this computer is read or kept).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Event, Key, Modifiers, Pos2, Rect};
use freedomoffice_server::config::Config;
use freedomoffice_server::store::{OrgRole, Store};

use super::*;
use crate::cloud::Http;

/// Test-only passwords for the throwaway accounts.
const PW: &str = "org-test-password";
const TEMP: &str = "temporary-password-20";

fn frame(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1300.0, 860.0))), events, ..Default::default() };
    let _ = ctx.run(input, |ctx| app.frame(ctx));
}

fn key(k: Key) -> Vec<Event> {
    vec![
        Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
        Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE },
    ]
}

/// Frames until `done` (background requests answer between them).
fn until(ctx: &egui::Context, app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
        frame(ctx, app, vec![]);
    }
}

struct Setup {
    server: freedomoffice_server::Running,
    url: String,
    dir: std::path::PathBuf,
    org_id: String,
    org_root: String,
}

fn setup() -> Setup {
    let dir = std::env::temp_dir().join(format!("fo-sheets-orgui-{}", freedomoffice_server::util::new_id()));
    let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
    let (org_id, org_root) = {
        let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        let ann = st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
        let ben = st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
        let dan = st.create_account("dan", "Dan New", "", TEMP, false, true).unwrap();
        let org = st.create_org("Lab Team", "test").unwrap();
        st.set_member(&org, &ann, OrgRole::Owner, "test").unwrap();
        st.set_member(&org, &ben, OrgRole::Member, "test").unwrap();
        st.set_member(&org, &dan, OrgRole::Member, "test").unwrap();
        (org.id.clone(), org.root_id.clone())
    };
    let server = freedomoffice_server::spawn(cfg).unwrap();
    let url = server.url();
    Setup { server, url, dir, org_id, org_root }
}

fn client(t: &Setup, user: &str, pw: &str) -> (egui::Context, App, Arc<ServerSession>) {
    let url = t.url.as_str();
    let ctx = egui::Context::default();
    let mut app = App::with_context(&ctx, 0, None);
    app.dialog = None;
    app.cloud.cache_override = Some(TestDir(t.dir.join(format!("{user}-cache-{}", crate::model::new_sheet_uid()))));
    let session = Arc::new(ServerSession::login(Box::new(Http::new()), url, user, pw, "test").unwrap());
    app.cloud_for_test(session.clone());
    (ctx, app, session)
}

#[test]
fn organisation_folders_are_places_in_open_and_save_as() {
    let t = setup();
    let (ctx, mut app, ann) = client(&t, "ann", PW);
    let place = Place::Server(0, ServerView::Org(0));
    assert_eq!(app.place_root(place), (Target::Folder(t.org_root.clone()), "Lab Team".to_string()));
    assert_eq!(app.place_root(Place::Server(0, ServerView::Org(5))).0, Target::MyFiles, "a stale organisation shows My files");

    // Save As into the organisation's folder from File > Save As.
    app.cloud.save_place = place;
    app.backstage = Some(Backstage::SaveAs);
    frame(&ctx, &mut app, vec![]);
    until(&ctx, &mut app, "the org folder is listed", |a| a.cloud.browser.root == Some(place) && !a.cloud.browser.loading);
    assert_eq!(app.cloud.browser.crumbs.last().map(|c| c.0.as_str()), Some("Lab Team"));
    let conn = app.browser_conn().unwrap();
    let folder = conn.folder_of(&app.cloud.browser.crumbs.last().unwrap().1);
    assert_eq!(folder.as_deref(), Some(t.org_root.as_str()), "the Save button saves into the org folder");
    app.cloud_save_as(conn, t.org_root.clone(), "Lab plan".into(), false);
    until(&ctx, &mut app, "saved", |a| !a.cloud.uploading);
    let doc = app.cloud.doc.clone().expect("the window is now the server file");
    assert_eq!((doc.name.as_str(), doc.parent.as_deref()), ("Lab plan.xlsx", Some(t.org_root.as_str())));
    // Saved to the server: the window joins the file's live session right away, as when it is
    // opened from the server (the local copy is where Open keeps it).
    assert!(doc.live.is_some() && app.live.is_some(), "live after Save As");
    assert!(doc.local.starts_with(&app.cloud.cache_override.as_ref().unwrap().0) && doc.local.ends_with(std::path::Path::new(&doc.item_id).join("Lab plan.xlsx")));
    assert_eq!(app.wb.path.as_ref(), Some(&doc.local));
    until(&ctx, &mut app, "ann is live", |a| a.live_quiet());
    assert!(app.cloud_title_suffix().contains("Saved to"), "{}", app.cloud_title_suffix());

    // Ben finds it in Open, under the organisation, and opens it.
    let (bctx, mut ben, _) = client(&t, "ben", PW);
    ben.cloud.open_place = place;
    ben.backstage = Some(Backstage::Open);
    frame(&bctx, &mut ben, vec![]);
    until(&bctx, &mut ben, "ben's listing", |a| a.cloud.browser.root == Some(place) && !a.cloud.browser.loading);
    let names: Vec<String> = ben.cloud.browser.items.iter().map(|i| i.name.clone()).collect();
    assert_eq!(names, ["Lab plan.xlsx"]);
    let item = ben.cloud.browser.items[0].clone();
    ben.cloud_open_item(item);
    until(&bctx, &mut ben, "opened", |a| a.cloud.doc.is_some());
    assert_eq!(ben.cloud.doc.as_ref().map(|d| d.item_id.clone()), Some(doc.item_id.clone()));
    until(&bctx, &mut ben, "ben is live", |a| a.live_quiet());

    // What Ben types reaches Ann's window (she never reopened the file).
    ben.select(4, 2, false);
    ben.begin_edit(Some("from ben".into()), super::super::EditMode::Enter);
    assert!(ben.commit_edit(false));
    let c5 = |a: &App| crate::numfmt::value_to_text(a.wb.sheets[0].display_value(4, 2));
    let end = Instant::now() + Duration::from_secs(20);
    while c5(&app) != "from ben" {
        assert!(Instant::now() < end, "Ben's change didn't reach Ann");
        frame(&bctx, &mut ben, vec![]);
        frame(&ctx, &mut app, vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = ann;
    drop(t.server);
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn sharing_with_an_organisation_from_the_share_dialog() {
    let t = setup();
    let (ctx, mut app, ann) = client(&t, "ann", PW);
    let item = ann.upload_new("root", "Mine.xlsx", b"mine", "fail").unwrap();
    let local = t.dir.join("mine");
    let doc = ann.open(&item, &local).unwrap();
    app.cloud.doc = Some(doc);
    app.cloud_open_share();
    frame(&ctx, &mut app, vec![]);
    frame(&ctx, &mut app, vec![]);
    let sh = app.cloud.share.as_mut().unwrap();
    assert_eq!((sh.org_pick, sh.org_edit), (0, false), "the first organisation, view only, by default");
    // (The Share button does this.)
    let s = ann.clone();
    let (id, org) = (item.id.clone(), t.org_id.clone());
    sh.busy = true;
    app.cloud_spawn(move || Event2::OrgShared(s.grant_org(&id, &org, false).map(|_| "Lab Team".to_string())));
    until(&ctx, &mut app, "shared", |a| a.cloud.share.as_ref().is_some_and(|s| !s.busy));
    assert_eq!(app.cloud.share.as_ref().and_then(|s| s.info.clone()).as_deref(), Some("Shared with everyone in Lab Team."));

    // Manage access lists the organisation; it can be removed there.
    app.cloud.share.as_mut().unwrap().tab = 2;
    until(&ctx, &mut app, "permissions", |a| a.cloud.share.as_ref().is_some_and(|s| s.perms.is_some() && !s.busy));
    let perms = app.cloud.share.as_ref().and_then(|s| s.perms.clone()).unwrap();
    let row = perms.iter().find(|p| p.id == format!("org:{}", t.org_id)).expect("org row");
    assert_eq!((row.who.as_str(), row.role_label(), row.removable()), ("Everyone in Lab Team", "Can view", true));
    let ben = ServerSession::login(Box::new(Http::new()), &t.url, "ben", PW, "test").unwrap();
    assert!(ben.item(&item.id).is_ok(), "members of the organisation can open it");
    ann.remove_permission(&item.id, &row.id).unwrap();
    assert!(ben.item(&item.id).is_err());
    drop(t.server);
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn a_temporary_password_asks_for_a_new_one() {
    let t = setup();
    let (ctx, mut app, dan) = client(&t, "dan", TEMP);
    assert!(app.cloud.servers[0].me.as_ref().is_some_and(|m| m.must_change_password && m.orgs.len() == 1));
    frame(&ctx, &mut app, vec![]);
    let f = app.cloud.password.as_ref().expect("the dialog opens by itself");
    assert!(f.forced && f.username == "dan");
    assert!(app.modal_open(), "the keyboard belongs to the dialog");

    // Typed into the form (temporary password first), never into the sheet.
    let before = app.wb.clone();
    frame(&ctx, &mut app, vec![Event::Text(TEMP.into())]);
    frame(&ctx, &mut app, key(Key::Tab));
    frame(&ctx, &mut app, vec![Event::Text("short".into())]);
    frame(&ctx, &mut app, key(Key::Tab));
    frame(&ctx, &mut app, vec![Event::Text("other".into())]);
    {
        let f = app.cloud.password.as_ref().unwrap();
        assert_eq!((f.old.as_str(), f.new.as_str(), f.repeat.as_str()), (TEMP, "short", "other"));
    }
    assert!(crate::live::compare(&before, &app.wb).is_empty() && app.edit.is_none());
    frame(&ctx, &mut app, key(Key::Enter));
    let f = app.cloud.password.as_ref().unwrap();
    assert_eq!(f.error.as_deref(), Some("The new passwords don't match."));
    assert!(!f.busy, "nothing was sent");

    // The server's policy is shown; the typed passwords are gone after the request.
    let f = app.cloud.password.as_mut().unwrap();
    f.repeat = "short".into();
    app.password_submit();
    assert!(app.cloud.password.as_ref().is_some_and(|f| f.busy && f.old.is_empty() && f.new.is_empty() && f.repeat.is_empty()));
    until(&ctx, &mut app, "policy answer", |a| a.cloud.password.as_ref().is_some_and(|f| !f.busy));
    let e = app.cloud.password.as_ref().and_then(|f| f.error.clone()).unwrap();
    assert!(e.contains("12"), "{e}");

    // "Not now": closed, and opened again when the server refuses the next request.
    app.cloud.password = None;
    dan.dismiss_password_change();
    frame(&ctx, &mut app, vec![]);
    assert!(app.cloud.password.is_none());
    assert!(dan.list(&Target::MyFiles).is_err());
    frame(&ctx, &mut app, vec![]);
    assert!(app.cloud.password.is_some(), "asked again");

    // The right passwords: done, and the server works again.
    let f = app.cloud.password.as_mut().unwrap();
    f.old = TEMP.into();
    f.new = "a-much-better-passphrase-77".into();
    f.repeat = f.new.clone();
    app.password_submit();
    until(&ctx, &mut app, "changed", |a| a.cloud.password.is_none());
    assert!(app.status.as_deref().is_some_and(|s| s.contains("password was changed")), "{:?}", app.status);
    until(&ctx, &mut app, "me again", |a| a.cloud.servers[0].me.as_ref().is_some_and(|m| !m.must_change_password));
    assert!(!dan.needs_password_change() && dan.list(&Target::MyFiles).is_ok());
    for _ in 0..3 {
        frame(&ctx, &mut app, vec![]);
    }
    assert!(app.cloud.password.is_none());

    // File > Account > Change password (not forced).
    app.server_change_password(0);
    assert!(app.cloud.password.as_ref().is_some_and(|f| !f.forced));
    frame(&ctx, &mut app, key(Key::Escape));
    assert!(app.cloud.password.is_none(), "Escape cancels");
    drop(t.server);
    let _ = std::fs::remove_dir_all(&t.dir);
}

use super::Event as Event2;

/// The latest content of a server file, cell (r, c) of the first sheet.
fn server_cell(s: &ServerSession, id: &str, dir: &std::path::Path, r: u32, c: u32) -> String {
    let doc = s.open(&s.item(id).unwrap(), &dir.join(format!("check-{}", crate::model::new_sheet_uid()))).unwrap();
    let wb = crate::io::load(&doc.local).unwrap();
    crate::numfmt::value_to_text(wb.sheets[0].display_value(r, c))
}

fn type_in(app: &mut App, r: u32, c: u32, text: &str) {
    app.select(r, c, false);
    app.begin_edit(Some(text.into()), super::super::EditMode::Enter);
    assert!(app.commit_edit(false));
}

/// A small workbook file for uploads.
fn save_sample(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("sample.xlsx");
    crate::io::save(&crate::model::Workbook::new(), &p).unwrap();
    p
}

/// Typing (and Ctrl+S) while Save As / Save a Copy is still uploading: those changes reach the
/// server; they are never dropped by joining the live session of a file that doesn't have them.
#[test]
fn edits_during_a_save_as_upload_are_not_lost() {
    let t = setup();
    for (case, ctrl_s) in [("typed", false), ("typed and saved", true)] {
        let (ctx, mut app, ann) = client(&t, "ann", PW);
        type_in(&mut app, 0, 0, "before");
        let name = format!("During {}", crate::model::new_sheet_uid());
        app.cloud_save_as(Conn::Server(ann.clone()), "root".into(), name, false);
        assert!(app.cloud.uploading);
        // While it uploads (its answer is handled on a later frame).
        type_in(&mut app, 1, 0, "while uploading");
        if ctrl_s {
            assert!(app.save());
        }
        until(&ctx, &mut app, "uploaded", |a| !a.cloud.uploading && a.cloud.doc.is_some());
        until(&ctx, &mut app, "the later changes are saved", |a| !a.cloud.uploading && !a.dirty);
        let doc = app.cloud.doc.clone().unwrap();
        assert!(app.live.is_none(), "not joined live with changes the session doesn't have ({case})");
        assert!(doc.local.exists(), "the local copy stays ({case})");
        let host = crate::cloud::server::host_of(&t.url).replace(':', "_");
        let second = app.cloud.cache_override.as_ref().unwrap().0.join(host).join(&doc.item_id);
        assert!(!second.exists(), "the download made for joining is removed when not joined ({case})");
        assert!(!app.cloud.upload_failed, "{case}: {:?}", app.status);
        assert_eq!(server_cell(&ann, &doc.item_id, &t.dir, 1, 0), "while uploading", "{case}");
        assert_eq!(server_cell(&ann, &doc.item_id, &t.dir, 0, 0), "before");
    }

    // Save a Copy (after a conflict) the same way.
    let (ctx, mut app, ann) = client(&t, "ann", PW);
    let item = ann.upload_new("root", "Original.xlsx", &std::fs::read(save_sample(&t.dir)).unwrap(), "fail").unwrap();
    let doc = ann.open(&item, &t.dir.join("orig")).unwrap();
    app.open_path(doc.local.clone());
    app.cloud.doc = Some(CloudDoc { live: None, ..doc });
    app.cloud_upload(UploadKind::Copy, true);
    type_in(&mut app, 2, 0, "copy edit");
    until(&ctx, &mut app, "copy uploaded and saved again", |a| !a.cloud.uploading && !a.dirty && a.cloud.doc.as_ref().is_some_and(|d| d.item_id != item.id));
    let copy = app.cloud.doc.clone().unwrap();
    assert!(app.live.is_none());
    assert_eq!(server_cell(&ann, &copy.item_id, &t.dir, 2, 0), "copy edit");

    // Nothing changed meanwhile: joined live at once.
    let (ctx, mut app, ann) = client(&t, "ann", PW);
    app.cloud_save_as(Conn::Server(ann.clone()), "root".into(), "Quiet.xlsx".into(), false);
    until(&ctx, &mut app, "uploaded", |a| !a.cloud.uploading && a.cloud.doc.is_some());
    assert!(app.live.is_some());
    drop(t.server);
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// A request to the test server's API as `who` (or nobody); status and JSON body.
fn api(t: &Setup, who: Option<&ServerSession>, method: &'static str, path: &str, body: Option<serde_json::Value>) -> (u16, serde_json::Value) {
    use crate::cloud::Transport;
    let mut req = crate::cloud::Request::new(method, format!("{}{path}", t.url));
    if let Some(s) = who {
        req = req.header("Authorization", format!("Bearer {}", s.token()));
    }
    if let Some(b) = &body {
        req = req.json(b);
    }
    let r = Http::new().send(req).unwrap();
    (r.status, serde_json::from_slice(&r.body).unwrap_or_default())
}

/// Ann (owner) takes Ben out of Lab Team.
fn remove_ben(t: &Setup, ann: &ServerSession) {
    assert_eq!(api(t, Some(ann), "DELETE", &format!("/api/orgs/{}/members/ben", t.org_id), None).0, 204);
}

/// Ann invites Ben back; he accepts with his existing account (as on the invite page).
fn readd_ben(t: &Setup, ann: &ServerSession) {
    let (st, v) = api(t, Some(ann), "POST", &format!("/api/orgs/{}/invites", t.org_id), Some(serde_json::json!({ "email": "" })));
    assert_eq!(st, 201, "{v}");
    let token = v["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let (st, v) = api(t, None, "POST", &format!("/api/invites/{token}"), Some(serde_json::json!({ "existing": true, "username": "ben", "password": PW })));
    assert_eq!(st, 201, "{v}");
}

fn org_names(app: &App) -> Vec<String> {
    app.cloud.servers[0].orgs().iter().map(|o| o.name.clone()).collect()
}

/// Open / Save As show the organisations the account is in now: one Ben was taken out of
/// goes when the browser is shown again (falling back to My files, with a message), and
/// comes back after he accepts an invite, without restarting.
#[test]
fn organisations_follow_membership_changes_when_the_browser_opens() {
    let t = setup();
    let (_actx, _a, ann) = client(&t, "ann", PW);
    ann.upload_new(&t.org_root, "Team.xlsx", &std::fs::read(save_sample(&t.dir)).unwrap(), "fail").unwrap();
    let (ctx, mut ben, _) = client(&t, "ben", PW);
    let place = Place::Server(0, ServerView::Org(0));
    ben.cloud.open_place = place;
    ben.backstage = Some(Backstage::Open);
    frame(&ctx, &mut ben, vec![]);
    until(&ctx, &mut ben, "the org folder is listed", |a| a.cloud.browser.root == Some(place) && !a.cloud.browser.loading && a.cloud.servers[0].checked.is_some() && !a.cloud.servers[0].checking);
    assert_eq!(ben.cloud.browser.items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["Team.xlsx"]);
    ben.backstage = None;
    frame(&ctx, &mut ben, vec![]);

    remove_ben(&t, &ann);
    ben.backstage = Some(Backstage::Open);
    frame(&ctx, &mut ben, vec![]);
    until(&ctx, &mut ben, "Lab Team is gone", |a| org_names(a).is_empty());
    assert_eq!(ben.cloud.open_place, Place::Server(0, ServerView::MyFiles), "back to My files");
    assert_eq!(ben.status.as_deref(), Some("You're no longer in Lab Team."));
    until(&ctx, &mut ben, "My files listed", |a| a.cloud.browser.root == Some(Place::Server(0, ServerView::MyFiles)) && !a.cloud.browser.loading);
    assert!(ben.cloud.browser.error.is_none(), "{:?}", ben.cloud.browser.error);

    // Back in: the place returns when Open is shown again.
    readd_ben(&t, &ann);
    ben.backstage = None;
    frame(&ctx, &mut ben, vec![]);
    ben.backstage = Some(Backstage::SaveAs);
    ben.cloud.save_place = Place::Server(0, ServerView::MyFiles);
    frame(&ctx, &mut ben, vec![]);
    until(&ctx, &mut ben, "Lab Team is back", |a| org_names(a) == ["Lab Team"]);

    // A listing that fails (the folder was shown before the change) re-reads the account.
    ben.cloud.save_place = place;
    until(&ctx, &mut ben, "the org folder is listed", |a| a.cloud.browser.root == Some(place) && !a.cloud.browser.loading);
    remove_ben(&t, &ann);
    let crumb = ben.cloud.browser.crumbs.last().unwrap().1.clone();
    ben.cloud_navigate(crumb, String::new(), false);
    until(&ctx, &mut ben, "Lab Team is gone again", |a| org_names(a).is_empty() && a.cloud.save_place == Place::Server(0, ServerView::MyFiles));
    until(&ctx, &mut ben, "My files listed", |a| a.cloud.browser.root == Some(Place::Server(0, ServerView::MyFiles)) && !a.cloud.browser.loading);

    // File > Account re-reads too, but at most every 30 s.
    readd_ben(&t, &ann);
    ben.backstage = Some(Backstage::Account);
    for _ in 0..5 {
        frame(&ctx, &mut ben, vec![]);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(org_names(&ben).is_empty() && !ben.cloud.servers[0].checking, "throttled");
    ben.cloud.servers[0].checked = Some(Instant::now() - Duration::from_secs(31));
    until(&ctx, &mut ben, "File > Account shows Lab Team", |a| org_names(a) == ["Lab Team"]);

    // A server that doesn't answer is asked again after 30 s too, not every frame.
    drop(t.server);
    ben.cloud.servers[0].checked = Some(Instant::now() - Duration::from_secs(31));
    until(&ctx, &mut ben, "the failed check answered", |a| !a.cloud.servers[0].checking && a.cloud.servers[0].checked.is_some_and(|c| c.elapsed() < Duration::from_secs(10)));
    for _ in 0..10 {
        frame(&ctx, &mut ben, vec![]);
        assert!(!ben.cloud.servers[0].checking, "no new request right after a failed one");
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// Taken out of the organisation while a workbook of it is open live: the session ends
/// (revoked) and the organisation's place goes at once; Save a copy offers My files on the
/// same server; after an invite is accepted, the place is back without a restart.
#[test]
fn a_revoked_live_session_refreshes_the_organisations() {
    let t = setup();
    let (_actx, _a, ann) = client(&t, "ann", PW);
    ann.upload_new(&t.org_root, "Team.xlsx", &std::fs::read(save_sample(&t.dir)).unwrap(), "fail").unwrap();
    let (ctx, mut ben, _) = client(&t, "ben", PW);
    let place = Place::Server(0, ServerView::Org(0));
    ben.cloud.open_place = place;
    ben.backstage = Some(Backstage::Open);
    frame(&ctx, &mut ben, vec![]);
    until(&ctx, &mut ben, "listed", |a| a.cloud.browser.root == Some(place) && !a.cloud.browser.loading && !a.cloud.servers[0].checking);
    let item = ben.cloud.browser.items[0].clone();
    ben.cloud_open_item(item);
    until(&ctx, &mut ben, "opened", |a| a.cloud.doc.is_some());
    until(&ctx, &mut ben, "ben is live", |a| a.live_quiet());
    ben.status = Some("PO Client B joined.".into());
    assert!(ben.cloud_share_enabled());

    remove_ben(&t, &ann);
    until(&ctx, &mut ben, "the session ended", |a| a.live_stopped().0.is_some());
    until(&ctx, &mut ben, "Lab Team is gone", |a| org_names(a).is_empty());
    assert_eq!(ben.cloud.open_place, Place::Server(0, ServerView::MyFiles));
    assert!(ben.cloud_share_off().is_some_and(|t| t.starts_with("Not connected")));
    // Save a copy: Save As on the same account's My files, not This PC.
    ben.cloud.save_place = Place::Local;
    ben.live_save_copy();
    assert!(ben.backstage == Some(Backstage::SaveAs));
    assert_eq!(ben.cloud.save_place, Place::Server(0, ServerView::MyFiles));
    frame(&ctx, &mut ben, vec![]);
    until(&ctx, &mut ben, "My files listed", |a| a.cloud.browser.root == Some(Place::Server(0, ServerView::MyFiles)) && !a.cloud.browser.loading);

    readd_ben(&t, &ann);
    ben.backstage = None;
    frame(&ctx, &mut ben, vec![]);
    ben.backstage = Some(Backstage::Open);
    frame(&ctx, &mut ben, vec![]);
    until(&ctx, &mut ben, "Lab Team is back", |a| org_names(a) == ["Lab Team"]);
    // A New workbook starts with a clean status bar.
    ben.status = Some("PO Client B joined.".into());
    ben.run_pending(super::super::Pending::New);
    assert!(ben.status.is_none() && ben.live.is_none());
    drop(t.server);
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[test]
fn link_trust_question_shows_decoded_and_raw_host_names() {
    use crate::cloud::open_url::host_name;
    let (text, warn) = LinkAsk::trust_text(&host_name("https://xn--80ak6aa92e.com"));
    assert_eq!(text, "This link points to аррӏе.com (xn--80ak6aa92e.com), a server you haven't signed in to. Only continue if you trust it.");
    assert!(warn.is_none());
    let (text, warn) = LinkAsk::trust_text(&host_name("https://files.example.com:8443/office"));
    assert_eq!(text, "This link points to files.example.com:8443, a server you haven't signed in to. Only continue if you trust it.");
    assert!(warn.is_none());
    let (text, warn) = LinkAsk::trust_text(&host_name("https://xn--99999999a.example.com"));
    assert!(text.starts_with("This link points to xn--99999999a.example.com, a server"), "{text}");
    assert!(warn.is_some_and(|w| w.contains("may imitate another name")));
}

#[test]
fn open_in_sheets_links_open_the_file_with_the_signed_in_account() {
    let t = setup();
    let (ctx, mut app, ann) = client(&t, "ann", PW);
    app.cloud_save_as(Conn::Server(ann.clone()), t.org_root.clone(), "Linked".into(), false);
    until(&ctx, &mut app, "saved", |a| !a.cloud.uploading);
    let doc = app.cloud.doc.clone().expect("saved to the server");
    let link = |server: &str| format!("freedomoffice://open?server={}&item={}", cloud::pct_encode(server), doc.item_id);

    // Ben is signed in to the server (the link spells its address differently): the file opens
    // and he joins its live session.
    let (bctx, mut ben, _) = client(&t, "ben", PW);
    ben.open_link(&link(&format!("{}/", t.url.to_uppercase())));
    until(&bctx, &mut ben, "opened from the link", |a| a.cloud.doc.is_some());
    assert_eq!(ben.cloud.doc.as_ref().map(|d| d.item_id.clone()), Some(doc.item_id.clone()));
    assert_eq!(ben.cloud.doc.as_ref().map(|d| d.name.clone()).as_deref(), Some("Linked.xlsx"));
    until(&bctx, &mut ben, "ben is live", |a| a.live_quiet());
    assert!(ben.live.is_some());

    // A file that doesn't exist: the usual error message, nothing opened.
    let (bctx2, mut ben2, _) = client(&t, "ben", PW);
    ben2.open_link(&format!("freedomoffice://open?server={}&item={}", cloud::pct_encode(&t.url), "0".repeat(32)));
    until(&bctx2, &mut ben2, "an error", |a| matches!(a.dialog, Some(Dialog::Message { .. })));
    assert!(ben2.cloud.doc.is_none());

    // A refused link: a message, nothing sent.
    ben2.dialog = None;
    ben2.open_link("freedomoffice://open?server=http%3A%2F%2Fevil.com&item=0123456789abcdef0123456789abcdef");
    frame(&bctx2, &mut ben2, vec![]);
    assert!(matches!(&ben2.dialog, Some(Dialog::Message { text, .. }) if text.contains("couldn't open the link")));
    assert!(ben2.cloud.opening.is_none());

    // Two accounts on that server: asked which one opens it.
    ben2.dialog = None;
    ben2.cloud.servers.push(ServerEntry::new(ann.clone(), None));
    ben2.open_link(&link(&t.url));
    frame(&bctx2, &mut ben2, vec![]);
    match &ben2.cloud.link_ask {
        Some(LinkAsk::Account { accounts, chosen: 0, .. }) => {
            assert_eq!(accounts.iter().map(|a| a.1.as_str()).collect::<Vec<_>>(), ["ben", "ann"]);
        }
        _ => panic!("expected the account question"),
    }
    assert!(ben2.cloud.opening.is_none(), "nothing opens before the choice");
    ben2.link_cancel();
    frame(&bctx2, &mut ben2, vec![]);
    assert!(ben2.cloud.link_ask.is_none() && ben2.cloud.opening.is_none());
    ben2.open_link(&link(&t.url));
    frame(&bctx2, &mut ben2, vec![]);
    if let Some(LinkAsk::Account { chosen, .. }) = &mut ben2.cloud.link_ask {
        *chosen = 1;
    }
    ben2.link_continue();
    until(&bctx2, &mut ben2, "opened as ann", |a| a.cloud.doc.is_some());
    assert_eq!(ben2.cloud.doc.as_ref().map(|d| d.item_id.clone()), Some(doc.item_id.clone()));

    // A server never signed in to: first a question naming the host; nothing is filled in or
    // sent before Continue.
    let cctx = egui::Context::default();
    let mut cara = App::with_context(&cctx, 0, None);
    cara.dialog = None;
    cara.no_cloud_for_test();
    cara.open_link(&link(&t.url));
    frame(&cctx, &mut cara, vec![]);
    let host = crate::cloud::server::host_of(&t.url);
    match &cara.cloud.link_ask {
        Some(LinkAsk::Trust { host: h, .. }) => {
            let (text, warn) = LinkAsk::trust_text(h);
            assert_eq!(text, format!("This link points to {host}, a server you haven't signed in to. Only continue if you trust it."));
            assert!(warn.is_none());
        }
        _ => panic!("expected the trust question"),
    }
    assert!(cara.backstage.is_none() && !cara.cloud.add_server.open && cara.cloud.link_wait.is_none());
    assert!(cara.modal_open());
    let sign_in = || {
        let session = Arc::new(ServerSession::login(Box::new(Http::new()), &t.url, "ben", PW, "test").unwrap());
        let me = session.me_info().ok();
        super::Event::ServerSignedIn(Ok((session, me)))
    };
    // Cancel: signing in to that server later on doesn't open the link.
    cara.link_cancel();
    frame(&cctx, &mut cara, vec![]);
    assert!(cara.cloud.link_ask.is_none() && cara.cloud.link_wait.is_none() && !cara.cloud.add_server.open);
    cara.cloud_event(sign_in());
    for _ in 0..20 {
        frame(&cctx, &mut cara, vec![]);
    }
    assert!(cara.cloud.opening.is_none() && cara.cloud.doc.is_none(), "a cancelled link never opens");
    cara.cloud.servers.clear();

    // Continue: File > Account with the address filled in; the file opens after signing in.
    cara.open_link(&link(&t.url));
    frame(&cctx, &mut cara, vec![]);
    cara.link_continue();
    frame(&cctx, &mut cara, vec![]);
    assert!(matches!(cara.backstage, Some(Backstage::Account)));
    let a = &cara.cloud.add_server;
    assert!(a.open && a.stage == 0 && a.url_input == t.url && a.password.is_empty() && a.username.is_empty());
    assert!(cara.cloud.servers.is_empty() && cara.cloud.opening.is_none() && cara.cloud.doc.is_none());
    cara.cloud_event(sign_in());
    until(&cctx, &mut cara, "opened after signing in", |a| a.cloud.doc.is_some());
    assert_eq!(cara.cloud.doc.as_ref().map(|d| d.item_id.clone()), Some(doc.item_id.clone()));
    assert!(cara.cloud.link_wait.is_none());

    let _ = ann;
    drop(t.server);
    let _ = std::fs::remove_dir_all(&t.dir);
}
