//! Comment threads (Excel 365 style): the purple marker on commented cells and its hover card,
//! the Comments pane (all threads of the sheet, filter, new comment, reply, resolve, edit,
//! delete, @mention of organisation members) and the commands behind them.
//!
//! A comment is a set of operations on `Sheet::threads` (rules in `cosync::threads`): they are
//! applied here like any command (one undo scope) and, in a live session, found by the
//! workbook comparison and sent like every other change. Mentions are plain `@Name` text.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};

use cosync::{Comment, Op, Thread};
use eframe::egui::{self, text::LayoutJob, Align2, Color32, Pos2, RichText, Sense, Stroke, StrokeKind, TextFormat, Vec2};

use super::theme;
use super::{App, Scope};
use crate::live::comments::{self, Who};
use crate::model::cell_name;

/// The marker on a commented cell: purple/blue, unlike the red of a note.
pub const MARKER: Color32 = Color32::from_rgb(0x6B, 0x4F, 0xD8);

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    #[default]
    All,
    Unresolved,
    Mine,
}

/// A new thread being written.
pub struct Draft {
    pub sheet: u64,
    pub r: u32,
    pub c: u32,
    pub text: String,
    focus: bool,
}

/// A comment being edited.
pub struct Editing {
    pub th: String,
    pub id: String,
    pub text: String,
}

#[derive(Default)]
pub struct CommentsUi {
    pub open: bool,
    pub filter: Filter,
    /// The thread to show and scroll to.
    pub focus: Option<String>,
    scroll_to: Option<String>,
    pub draft: Option<Draft>,
    pub replies: HashMap<String, String>,
    pub editing: Option<Editing>,
    /// People of the document's organisation, for @mentions: (user name, display name).
    pub members: Vec<(String, String)>,
    members_item: Option<String>,
    members_rx: Option<Receiver<Vec<(String, String)>>>,
}

/// What the pane asks for (collected while drawing, done afterwards).
enum Act {
    Post,
    Cancel,
    Reply(String),
    Done(String, bool),
    Delete(String, String),
    Edit(String, String, String),
    Goto(String, u32, u32),
}

/// `Today 14:05`, `Nov 3, 14:05`, `Nov 3 2025, 14:05`: in the viewer's own time zone.
pub fn when(ms: i64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let Some(t) = Local.timestamp_millis_opt(ms).single() else { return String::new() };
    let now = Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("Today %H:%M").to_string()
    } else if t.year() == now.year() {
        t.format("%b %-d, %H:%M").to_string()
    } else {
        t.format("%b %-d %Y, %H:%M").to_string()
    }
}

fn initials(name: &str) -> String {
    let mut it = name.split_whitespace().filter_map(|w| w.chars().next());
    let (a, b) = (it.next(), it.next());
    a.into_iter().chain(b).filter(|c| c.is_alphanumeric()).flat_map(char::to_uppercase).take(2).collect()
}

fn author_name(c: &Comment) -> &str {
    if !c.name.is_empty() {
        &c.name
    } else if !c.by.is_empty() {
        &c.by
    } else {
        "Guest"
    }
}

/// The comment text as a layout job with the mentioned people highlighted.
fn text_job(text: &str, members: &[(String, String)], size: f32, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat { font_id: theme::ui_font(size), color, ..Default::default() };
    let mention = TextFormat { font_id: theme::ui_bold(size), color: MARKER, background: MARKER.gamma_multiply(0.14), ..Default::default() };
    let mut at = 0;
    for (a, b) in comments::mention_spans(text, members) {
        job.append(&text[at..a], 0.0, plain.clone());
        job.append(&text[a..b], 0.0, mention.clone());
        at = b;
    }
    job.append(&text[at..], 0.0, plain);
    job.wrap.max_width = f32::INFINITY;
    job
}

/// The `@fragment` being typed at the end of `text` and the people it could finish.
fn suggestions(text: &str, members: &[(String, String)]) -> Option<(usize, Vec<(String, String)>)> {
    let at = text.rfind('@')?;
    if at > 0 && !text[..at].ends_with(char::is_whitespace) {
        return None;
    }
    let frag = &text[at + 1..];
    if frag.contains('\n') || frag.chars().count() > 30 {
        return None;
    }
    let f = frag.to_lowercase();
    let list: Vec<(String, String)> = members
        .iter()
        .filter(|(u, n)| f.is_empty() || u.to_lowercase().starts_with(&f) || n.to_lowercase().starts_with(&f) || n.to_lowercase().split_whitespace().any(|w| w.starts_with(&f)))
        .filter(|(u, n)| !(n.to_lowercase() == f || u.to_lowercase() == f) || n.chars().count() > frag.chars().count())
        .take(5)
        .cloned()
        .collect();
    (!list.is_empty()).then_some((at, list))
}

impl App {
    /// Who is commenting: the signed-in person in a live session, else the person at this
    /// computer (the owner of their own file).
    pub(crate) fn comment_who(&self) -> Who {
        if let Some(me) = self.live.as_ref().and_then(|l| l.me.as_ref()) {
            let name = if me.agent && !me.name.ends_with("(agent)") { format!("{} (agent)", me.name) } else { me.name.clone() };
            return Who { user: me.user.clone(), name, manager: me.manager };
        }
        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "me".into());
        Who { name: user.clone(), user, manager: true }
    }

    /// Whether the person may comment now; says why not in the status bar.
    pub(crate) fn comments_blocked(&mut self) -> bool {
        if self.live.is_some() {
            if self.live_read_only() {
                return true;
            }
            if self.live.as_ref().is_some_and(|l| !l.comments) {
                self.status = Some("This server can't share comments yet. Ask for the server to be updated.".into());
                return true;
            }
        }
        false
    }

    /// Ctrl+Alt+M, the right-click menu and the ribbon: a new comment on the active cell (or the
    /// thread already there).
    pub fn comment_new(&mut self) {
        self.commit_edit(false);
        let (r, c) = self.active();
        let si = self.wb.active;
        self.comments.open = true;
        if let Some(t) = self.wb.sheets[si].threads.iter().find(|t| (t.r, t.c) == (r, c)) {
            self.comments.focus = Some(t.id.clone());
            self.comments.scroll_to = Some(t.id.clone());
            self.comments.draft = None;
            return;
        }
        if self.comments_blocked() {
            return;
        }
        self.comments.draft = Some(Draft { sheet: self.wb.sheets[si].uid, r, c, text: String::new(), focus: true });
    }

    pub fn comments_toggle(&mut self) {
        self.comments.open = !self.comments.open;
    }

    /// Applies comment operations as one command.
    pub(crate) fn comment_apply(&mut self, ops: Vec<Op>) {
        let Some(si) = ops.first().and_then(|o| o.sheet()).and_then(|s| crate::live::index_of(&self.wb, s)) else { return };
        self.push_undo(&[Scope::Sheet(si)]);
        for op in &ops {
            cosync::apply_thread_op(&mut self.wb.sheets[si].threads, op);
        }
        self.dirty = true;
    }

    /// Opens the pane on a thread (a click on a cell's marker).
    pub(crate) fn comments_show(&mut self, id: &str) {
        self.comments.open = true;
        self.comments.focus = Some(id.to_string());
        self.comments.scroll_to = Some(id.to_string());
    }

    /// People for @mentions: the organisation's members (fetched once per document), the
    /// people in the session, and everyone who wrote a comment here.
    fn comment_members(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self.comments.members.clone();
        let mut add = |u: &str, n: &str| {
            if !u.is_empty() && !out.iter().any(|(x, _)| x.eq_ignore_ascii_case(u)) {
                out.push((u.to_string(), if n.is_empty() { u.to_string() } else { n.to_string() }));
            }
        };
        if let Some(l) = &self.live {
            for p in l.peers.values() {
                add(&p.peer.user, &p.peer.name);
            }
            if let Some(me) = &l.me {
                add(&me.user, &me.name);
            }
        }
        for t in self.wb.sheets.iter().flat_map(|s| &s.threads) {
            for c in &t.comments {
                add(&c.by, &c.name);
            }
        }
        out.sort_by_key(|a| a.1.to_lowercase());
        out
    }

    /// Fetches the organisation's members for this document when the pane is open.
    fn comments_load_members(&mut self) {
        if let Some(rx) = &self.comments.members_rx {
            if let Ok(list) = rx.try_recv() {
                self.comments.members = list;
                self.comments.members_rx = None;
            }
            return;
        }
        let Some((server, item)) = self.live_server() else { return };
        if self.comments.members_item.as_deref() == Some(item.as_str()) {
            return;
        }
        self.comments.members_item = Some(item.clone());
        let (tx, rx) = channel();
        self.comments.members_rx = Some(rx);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(server.document_members(&item).unwrap_or_default());
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    // ------------------------------------------------------------------
    // The marker's hover card
    // ------------------------------------------------------------------

    /// The card shown over a commented cell: the thread read-only.
    pub(super) fn paint_thread_card(&self, lp: &egui::Painter, (t, rx): (f32, f32), th: &Thread) {
        let members = self.comment_members();
        let mut job = LayoutJob::default();
        job.wrap.max_width = 250.0;
        for (i, c) in th.comments.iter().enumerate().take(6) {
            if i > 0 {
                job.append("\n", 0.0, TextFormat::default());
            }
            job.append(author_name(c), 0.0, TextFormat { font_id: theme::ui_bold(12.0), color: Color32::BLACK, ..Default::default() });
            job.append(&format!("  {}\n", when(c.at)), 0.0, TextFormat { font_id: theme::ui_font(10.5), color: Color32::from_gray(110), ..Default::default() });
            let body = text_job(&c.text, &members, 12.0, Color32::BLACK);
            for s in body.sections {
                job.append(&body.text[s.byte_range.clone()], 0.0, s.format);
            }
            job.append("\n", 0.0, TextFormat::default());
        }
        if th.comments.len() > 6 {
            job.append(&format!("... {} more", th.comments.len() - 6), 0.0, TextFormat { font_id: theme::ui_font(11.0), color: Color32::from_gray(110), ..Default::default() });
        }
        if th.done {
            job.append("Resolved", 0.0, TextFormat { font_id: theme::ui_bold(11.0), color: Color32::from_rgb(0x21, 0x73, 0x46), ..Default::default() });
        }
        let g = lp.layout_job(job);
        let origin = Pos2::new(rx + 10.0, t - 2.0);
        let bx = egui::Rect::from_min_size(origin, g.size() + Vec2::new(18.0, 12.0));
        lp.rect_filled(bx.translate(Vec2::new(2.0, 2.0)), 4.0, Color32::from_black_alpha(30));
        lp.rect_filled(bx, 4.0, Color32::WHITE);
        lp.rect_stroke(bx, 4.0, Stroke::new(1.0, Color32::from_gray(190)), StrokeKind::Inside);
        lp.rect_filled(egui::Rect::from_min_size(bx.min, Vec2::new(4.0, bx.height())), 2.0, MARKER);
        lp.galley(origin + Vec2::new(12.0, 6.0), g, Color32::BLACK);
    }

    // ------------------------------------------------------------------
    // The Comments pane
    // ------------------------------------------------------------------

    pub(super) fn comments_pane(&mut self, ctx: &egui::Context) {
        if !self.comments.open || self.backstage.is_some() {
            return;
        }
        self.comments_load_members();
        let members = self.comment_members();
        let who = self.comment_who();
        let si = self.wb.active;
        let blocked_why = self.comments_why_not();
        let sheet_uid = self.wb.sheets[si].uid;
        let mut threads: Vec<Thread> = self.wb.sheets[si]
            .threads
            .iter()
            .filter(|t| match self.comments.filter {
                Filter::All => true,
                Filter::Unresolved => !t.done,
                Filter::Mine => !who.user.is_empty() && t.comments.iter().any(|c| c.by == who.user),
            })
            .cloned()
            .collect();
        threads.sort_by(|a, b| (a.r, a.c, a.comments[0].at).cmp(&(b.r, b.c, b.comments[0].at)));
        let total = self.wb.sheets[si].threads.len();
        let mut acts: Vec<Act> = Vec::new();
        let mut close = false;
        let mut filter = self.comments.filter;
        egui::SidePanel::right("comments_pane")
            .resizable(true)
            .default_width(330.0)
            .min_width(260.0)
            .frame(egui::Frame::NONE.fill(theme::window()).inner_margin(10.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Comments").size(16.0));
                    ui.label(RichText::new(format!("{total}")).size(12.0).color(theme::text_dim()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").on_hover_text("Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    for (f, label) in [(Filter::All, "All"), (Filter::Unresolved, "Unresolved"), (Filter::Mine, "Mine")] {
                        if ui.selectable_label(filter == f, label).clicked() {
                            filter = f;
                        }
                    }
                });
                if let Some(why) = &blocked_why {
                    ui.label(RichText::new(why).size(12.0).color(theme::text_dim()));
                } else if self.comments.draft.is_none() && ui.button("New comment").on_hover_text("Ctrl+Alt+M").clicked() {
                    self.comment_new();
                }
                ui.separator();
                if let Some(d) = &mut self.comments.draft {
                    if d.sheet != sheet_uid {
                        ui.label(RichText::new("The new comment is on another sheet.").size(12.0).color(theme::text_dim()));
                    }
                    ui.label(RichText::new(format!("New comment on {}", cell_name(d.r, d.c))).strong());
                    let submit = input_box(ui, "comment_draft", &mut d.text, "Start a conversation. Use @ to mention someone.", &members, std::mem::take(&mut d.focus));
                    ui.horizontal(|ui| {
                        let ok = !d.text.trim().is_empty() && d.sheet == sheet_uid;
                        if ui.add_enabled(ok, egui::Button::new("Post")).on_hover_text("Ctrl+Enter").clicked() || (submit && ok) {
                            acts.push(Act::Post);
                        }
                        if ui.button("Cancel").clicked() {
                            acts.push(Act::Cancel);
                        }
                    });
                    ui.separator();
                }
                if threads.is_empty() && self.comments.draft.is_none() {
                    let msg = if total == 0 { "No comments on this sheet yet.\nSelect a cell and press Ctrl+Alt+M." } else { "No comments match this filter." };
                    ui.label(RichText::new(msg).size(12.5).color(theme::text_dim()));
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for t in &threads {
                        self.thread_ui(ui, t, &who, &members, blocked_why.is_none(), &mut acts);
                        ui.add_space(6.0);
                    }
                });
            });
        self.comments.filter = filter;
        if close {
            self.comments.open = false;
        }
        for a in acts {
            self.comments_act(a);
        }
    }

    /// Why comments can't be changed now, if so (for the pane; nothing is said in the status bar).
    fn comments_why_not(&self) -> Option<String> {
        let l = self.live.as_ref()?;
        if !l.editor {
            Some("You have view-only access, so you can read comments but not add or change them.".into())
        } else if !l.comments && l.me.is_some() {
            Some("This server can't share comments yet. Comments in the file are shown, but changes can't be sent.".into())
        } else if !l.comments {
            Some("Comments can be added once the connection to the server is up.".into())
        } else {
            None
        }
    }

    fn thread_ui(&mut self, ui: &mut egui::Ui, t: &Thread, who: &Who, members: &[(String, String)], can_write: bool, acts: &mut Vec<Act>) {
        let focused = self.comments.focus.as_deref() == Some(t.id.as_str());
        let stroke = if focused { Stroke::new(2.0, MARKER) } else { Stroke::new(1.0, theme::border()) };
        let dim = if t.done { theme::text_dim() } else { theme::text() };
        let frame = egui::Frame::NONE.stroke(stroke).corner_radius(4.0).inner_margin(8.0);
        let resp = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let label = RichText::new(cell_name(t.r, t.c)).strong().color(MARKER);
                if ui.add(egui::Label::new(label).sense(Sense::click())).on_hover_text("Go to the cell").clicked() {
                    acts.push(Act::Goto(t.id.clone(), t.r, t.c));
                }
                if t.done {
                    ui.label(RichText::new("✔ Resolved").size(11.5).color(Color32::from_rgb(0x21, 0x73, 0x46)));
                }
            });
            for (i, c) in t.comments.iter().enumerate() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let peer = cosync::Peer { user: c.by.clone(), name: c.name.clone(), agent: c.name.ends_with("(agent)"), ..Default::default() };
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 13.0, super::live_ui::user_color(&peer));
                    ui.painter().text(rect.center(), Align2::CENTER_CENTER, initials(author_name(c)), theme::ui_bold(11.0), Color32::WHITE);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(author_name(c)).strong().color(dim));
                        let edited = if c.edited != 0 { "  (edited)" } else { "" };
                        ui.label(RichText::new(format!("{}{edited}", when(c.at))).size(11.0).color(theme::text_dim()));
                    });
                    if can_write && (comments::can_edit(who, c) || comments::can_delete(who, c)) {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.menu_button("⋯", |ui| {
                                if comments::can_edit(who, c) && ui.button("Edit").clicked() {
                                    self.comments.editing = Some(Editing { th: t.id.clone(), id: c.id.clone(), text: c.text.clone() });
                                    ui.close();
                                }
                                if comments::can_delete(who, c) && ui.button(if i == 0 { "Delete thread" } else { "Delete" }).clicked() {
                                    acts.push(Act::Delete(t.id.clone(), c.id.clone()));
                                    ui.close();
                                }
                            });
                        });
                    }
                });
                if self.comments.editing.as_ref().is_some_and(|e| e.id == c.id) {
                    let mut finish: Option<bool> = None;
                    if let Some(e) = &mut self.comments.editing {
                        let submit = input_box(ui, &format!("edit_{}", c.id), &mut e.text, "", members, false);
                        ui.horizontal(|ui| {
                            let ok = !e.text.trim().is_empty();
                            if ui.add_enabled(ok, egui::Button::new("Save")).clicked() || (submit && ok) {
                                finish = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                finish = Some(false);
                            }
                        });
                    }
                    match finish {
                        Some(true) => {
                            if let Some(e) = self.comments.editing.take() {
                                acts.push(Act::Edit(e.th, e.id, e.text));
                            }
                        }
                        Some(false) => self.comments.editing = None,
                        None => {}
                    }
                } else {
                    let mut job = text_job(&c.text, members, 13.0, dim);
                    job.wrap.max_width = (ui.available_width() - 4.0).max(60.0);
                    ui.label(job);
                }
            }
            if can_write {
                ui.add_space(6.0);
                let draft = self.comments.replies.entry(t.id.clone()).or_default();
                let submit = input_box(ui, &format!("reply_{}", t.id), draft, "Reply", members, false);
                ui.horizontal(|ui| {
                    let ok = !draft.trim().is_empty();
                    if ui.add_enabled(ok, egui::Button::new("Reply")).on_hover_text("Ctrl+Enter").clicked() || (submit && ok) {
                        acts.push(Act::Reply(t.id.clone()));
                    }
                    if ui.button(if t.done { "Reopen" } else { "Resolve" }).clicked() {
                        acts.push(Act::Done(t.id.clone(), !t.done));
                    }
                });
            }
        });
        if self.comments.scroll_to.as_deref() == Some(t.id.as_str()) {
            ui.scroll_to_rect(resp.response.rect, Some(egui::Align::Center));
            self.comments.scroll_to = None;
        }
    }

    fn comments_act(&mut self, a: Act) {
        match a {
            Act::Cancel => self.comments.draft = None,
            Act::Goto(id, r, c) => {
                self.comments.focus = Some(id);
                self.select(r, c, false);
                self.scroll_into_view(r, c);
            }
            Act::Post => {
                let Some(d) = self.comments.draft.take() else { return };
                if let Some(id) = self.comment_add_on(d.sheet, d.r, d.c, &d.text) {
                    self.comments.focus = Some(id.clone());
                    self.comments.scroll_to = Some(id);
                } else {
                    self.comments.draft = Some(d);
                }
            }
            Act::Reply(th) => {
                let text = self.comments.replies.get(&th).cloned().unwrap_or_default();
                if self.comment_reply(&th, &text) {
                    self.comments.replies.remove(&th);
                }
            }
            Act::Done(th, done) => {
                self.comment_resolve(&th, done);
            }
            Act::Delete(th, id) => {
                if self.comment_delete(&th, &id) {
                    self.comments.replies.remove(&th);
                }
            }
            Act::Edit(th, id, text) => {
                self.comment_edit(&th, &id, &text);
            }
        }
    }

    // ------------------------------------------------------------------
    // Commands (the pane, and tests)
    // ------------------------------------------------------------------

    fn sheet_uid(&self) -> u64 {
        self.wb.sheets[self.wb.active].uid
    }

    /// A new thread on a cell of the active sheet; its id. None when comments are blocked or the text is empty.
    pub(crate) fn comment_add(&mut self, r: u32, c: u32, text: &str) -> Option<String> {
        let s = self.sheet_uid();
        self.comment_add_on(s, r, c, text)
    }

    fn comment_add_on(&mut self, s: u64, r: u32, c: u32, text: &str) -> Option<String> {
        if text.trim().is_empty() || self.comments_blocked() {
            return None;
        }
        let op = comments::add_thread(s, r, c, &self.comment_who(), text.trim());
        let Op::ThreadAdd { th, .. } = &op else { return None };
        let id = th.id.clone();
        self.comment_apply(vec![op]);
        Some(id)
    }

    pub(crate) fn comment_reply(&mut self, th: &str, text: &str) -> bool {
        if text.trim().is_empty() || self.comments_blocked() {
            return false;
        }
        let op = comments::reply(self.sheet_uid(), th, &self.comment_who(), text.trim());
        self.comment_apply(vec![op]);
        true
    }

    pub(crate) fn comment_edit(&mut self, th: &str, id: &str, text: &str) -> bool {
        if text.trim().is_empty() || self.comments_blocked() {
            return false;
        }
        let who = self.comment_who();
        let own = self.wb.sheets[self.wb.active].threads.iter().find(|t| t.id == th).and_then(|t| t.comments.iter().find(|c| c.id == id)).is_some_and(|c| comments::can_edit(&who, c));
        if !own {
            self.status = Some("You can only edit your own comments.".into());
            return false;
        }
        self.comment_apply(vec![comments::edit(self.sheet_uid(), th, id, &who, text.trim())]);
        true
    }

    /// Deletes a comment (the first one takes the thread). Authors delete their own; managers any.
    pub(crate) fn comment_delete(&mut self, th: &str, id: &str) -> bool {
        if self.comments_blocked() {
            return false;
        }
        let who = self.comment_who();
        let ok = self.wb.sheets[self.wb.active].threads.iter().find(|t| t.id == th).and_then(|t| t.comments.iter().find(|c| c.id == id)).is_some_and(|c| comments::can_delete(&who, c));
        if !ok {
            self.status = Some("You can only delete your own comments.".into());
            return false;
        }
        self.comment_apply(vec![comments::delete(self.sheet_uid(), th, id, &who)]);
        true
    }

    pub(crate) fn comment_resolve(&mut self, th: &str, done: bool) -> bool {
        if self.comments_blocked() {
            return false;
        }
        self.comment_apply(vec![comments::resolve(self.sheet_uid(), th, done)]);
        true
    }

    /// A click on the marker in the corner of a commented cell opens the thread.
    pub(super) fn comments_marker_click(&mut self, ui: &egui::Ui, lay: &super::grid::Layout) {
        if self.edit.is_some() || !ui.input(|i| i.pointer.primary_clicked()) {
            return;
        }
        let Some(p) = ui.input(|i| i.pointer.interact_pos()) else { return };
        let s = 12.0 * self.zoom.max(0.7);
        let si = self.wb.active;
        let hit = self.wb.sheets[si].threads.iter().find(|t| {
            let (Some((top, _)), Some((_, rx))) = (lay.row_span(t.r), lay.col_span(t.c)) else { return false };
            lay.cells.contains(p) && p.x >= rx - s - 1.0 && p.x <= rx && p.y >= top && p.y <= top + s && (p.x - (rx - s - 1.0)) >= (p.y - top)
        });
        if let Some(t) = hit.map(|t| t.id.clone()) {
            self.comments_show(&t);
        }
    }
}

/// A multi-line box with @mention suggestions below it; true on Ctrl+Enter.
fn input_box(ui: &mut egui::Ui, id: &str, text: &mut String, hint: &str, members: &[(String, String)], focus: bool) -> bool {
    let out = egui::TextEdit::multiline(text).id_salt(id).hint_text(hint).desired_rows(2).desired_width(f32::INFINITY).show(ui);
    if focus {
        out.response.request_focus();
    }
    let submit = out.response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && (i.modifiers.ctrl || i.modifiers.command));
    if out.response.has_focus() || out.response.lost_focus() || focus {
        if let Some((at, list)) = suggestions(text, members) {
            let mut pick: Option<String> = None;
            ui.vertical(|ui| {
                for (user, name) in &list {
                    let label = if name.eq_ignore_ascii_case(user) { name.clone() } else { format!("{name}  (@{user})") };
                    if ui.selectable_label(false, label).clicked() {
                        pick = Some(if name.is_empty() { user.clone() } else { name.clone() });
                    }
                }
            });
            if let Some(n) = pick {
                text.truncate(at);
                text.push_str(&format!("@{n} "));
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), out.response.id) {
                    let end = egui::text::CCursor::new(text.chars().count());
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                    st.store(ui.ctx(), out.response.id);
                }
                out.response.request_focus();
            }
        }
    }
    submit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_finish_a_mention_being_typed() {
        let members = vec![("ann".to_string(), "Ann Lee".to_string()), ("ben".to_string(), "Ben Ray".to_string())];
        assert_eq!(suggestions("hi @", &members).map(|(a, l)| (a, l.len())), Some((3, 2)));
        assert_eq!(suggestions("hi @le", &members).map(|(_, l)| l[0].0.clone()), Some("ann".into()));
        assert_eq!(suggestions("hi @b", &members).map(|(_, l)| l[0].0.clone()), Some("ben".into()));
        assert!(suggestions("mail me@ann", &members).is_none(), "an @ inside a word is not a mention");
        assert!(suggestions("no mention", &members).is_none());
        assert!(suggestions("hi @Ann Lee", &members).is_none(), "already complete");
    }

    #[test]
    fn times_are_in_the_local_zone() {
        let now = chrono::Local::now().timestamp_millis();
        assert!(when(now).starts_with("Today "));
        assert!(when(1_000_000_000_000).contains("2001"));
    }

    #[test]
    fn initials_of_names() {
        assert_eq!(initials("Ann Lee"), "AL");
        assert_eq!(initials("claude (agent)"), "C");
        assert_eq!(initials("x"), "X");
    }
}
