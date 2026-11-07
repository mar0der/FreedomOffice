//! The journal of a live session: what a client has made and the server has not acknowledged,
//! kept in a small file so an app or agent that closes or crashes (offline, or in the middle of
//! sending) can pick the changes up again.
//!
//! One file per (server, account, document), written atomically (temp file, fsync, rename) with
//! mode 0600 and removed once everything is acknowledged. It holds the session's bookkeeping
//! (`SessionState`: client id, sequence number, the batch in flight and the buffered changes)
//! and where the document came from; never a password or a token (those stay in the sign-in
//! stores). The same code is used by the app and by `fsheets`.
//!
//! Resuming: the document is downloaded as for a first join (version V, sequence number S).
//! The journal's changes were made at sequence number B (>= S in practice): the session joins
//! with the journal's client id, replays the log from S, and when it reaches B puts the journaled
//! changes back (`Session::restore`); the replay after B then treats them exactly like a
//! reconnect, so a batch that did get committed before the crash is recognised by (cid, batch
//! id) and applied once. If B < S (a snapshot newer than the journal exists) the changes are
//! rebased on the new version like after a long offline period (`Session::rebase_on_reload`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::SessionState;

pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub v: u32,
    /// Who wrote it ("sheets" or "fsheets"), for messages.
    #[serde(default)]
    pub app: String,
    /// Server base URL, account name and document id: the identity of the journal.
    pub url: String,
    pub user: String,
    pub item: String,
    /// The document's name (for messages).
    pub name: String,
    /// The version the session started from and the sequence number it contained.
    pub version: i64,
    pub file_seq: u64,
    /// The local working copy of the document (the app's cache), when there is one.
    #[serde(default)]
    pub local: String,
    /// The sheet ids of that version ("id:pos,...", see the server's `X-Live-Sheets`).
    #[serde(default)]
    pub sheets: String,
    /// Seconds since the epoch of the last write.
    pub saved_at: u64,
    pub session: SessionState,
    /// How many separate changes (commands, edits) the batch in flight / the buffer hold.
    #[serde(default)]
    pub inflight_changes: usize,
    #[serde(default)]
    pub buffer_changes: usize,
}

impl Journal {
    /// A journal for a session; `saved_at` is set when it is written.
    pub fn new(app: &str, url: &str, user: &str, item: &str, name: &str, session: SessionState) -> Self {
        Journal {
            v: FORMAT,
            app: app.to_string(),
            url: url.to_string(),
            user: user.to_string(),
            item: item.to_string(),
            name: name.to_string(),
            version: 0,
            file_seq: 0,
            local: String::new(),
            sheets: String::new(),
            saved_at: 0,
            session,
            inflight_changes: 0,
            buffer_changes: 0,
        }
    }

    /// Number of unacknowledged operations.
    pub fn pending_ops(&self) -> usize {
        self.session.inflight.as_ref().map_or(0, |b| b.ops.len()) + self.session.buffer.len()
    }

    /// Number of unacknowledged changes (at least one when there are operations).
    pub fn pending_changes(&self) -> usize {
        let n = self.inflight_changes + self.buffer_changes;
        if n == 0 && self.pending_ops() > 0 { 1 } else { n }
    }

    pub fn has_pending(&self) -> bool {
        self.pending_ops() > 0
    }

    /// File name for a (server, account, document): a hash, so nothing in it needs escaping.
    pub fn file_name(url: &str, user: &str, item: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in format!("{}\n{user}\n{item}", url.trim_end_matches('/')).bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{h:016x}.journal.json")
    }

    pub fn path_in(&self, dir: &Path) -> PathBuf {
        dir.join(Self::file_name(&self.url, &self.user, &self.item))
    }

    /// Writes the journal atomically into `dir` (created, mode 0700, when missing).
    pub fn write(&mut self, dir: &Path) -> std::io::Result<()> {
        self.saved_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let data = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let path = self.path_in(dir);
        write_private(&path, &data)
    }

    pub fn read(path: &Path) -> Result<Journal, String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let j: Journal = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        if j.v != FORMAT {
            return Err(format!("journal format {} is not supported", j.v));
        }
        Ok(j)
    }

    /// The journal of this document, if one exists.
    pub fn find(dir: &Path, url: &str, user: &str, item: &str) -> Option<Journal> {
        Self::read(&dir.join(Self::file_name(url, user, item))).ok().filter(|j| j.has_pending())
    }

    /// Every journal with unacknowledged changes in `dir`, oldest first.
    pub fn all(dir: &Path) -> Vec<Journal> {
        let mut out: Vec<Journal> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".journal.json")))
            .filter_map(|p| Self::read(&p).ok())
            .filter(|j| j.has_pending())
            .collect();
        out.sort_by_key(|j| j.saved_at);
        out
    }

    /// Removes this document's journal (nothing is pending any more, or the changes were kept
    /// elsewhere).
    pub fn remove(dir: &Path, url: &str, user: &str, item: &str) {
        let _ = std::fs::remove_file(dir.join(Self::file_name(url, user, item)));
    }
}

/// Creates `path`'s directory (mode 0700) and writes the file atomically with mode 0600.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent().filter(|d| !d.exists()) {
        std::fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    let r = f.write_all(data).and_then(|_| f.sync_data());
    drop(f);
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Session;
    use crate::server::{Commit, Log};
    use crate::{CellData, Op, Val};

    fn dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cosync-journal-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
    }

    fn put(r: u32, t: &str) -> Op {
        Op::Content { s: 1, r, c: 0, v: Some(Box::new(CellData { v: Val::Text(t.into()), ..Default::default() })), adj: vec![] }
    }

    #[test]
    fn a_journal_round_trips_privately_and_holds_no_secret() {
        let d = dir("rt");
        let mut s = Session::new("c1", 4);
        s.local(vec![put(0, "a")]);
        s.offline();
        s.local(vec![put(1, "b")]);
        let mut j = Journal::new("test", "https://x.example", "ann", "item1", "Book.xlsx", s.state());
        j.inflight_changes = 1;
        j.buffer_changes = 1;
        j.write(&d).unwrap();
        let back = Journal::find(&d, "https://x.example/", "ann", "item1").expect("found by its identity");
        assert_eq!(back.session, s.state());
        assert_eq!((back.pending_ops(), back.pending_changes()), (2, 2));
        assert!(Journal::find(&d, "https://x.example", "ben", "item1").is_none());
        let raw = std::fs::read_to_string(j.path_in(&d)).unwrap();
        assert!(!raw.to_lowercase().contains("token") && !raw.to_lowercase().contains("password"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(j.path_in(&d)).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no temp file is left behind");
        assert_eq!(Journal::all(&d).len(), 1);
        Journal::remove(&d, "https://x.example", "ann", "item1");
        assert!(Journal::all(&d).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A batch the server committed before the program died (its ack never arrived) is
    /// recognised when the journal is resumed, and the buffered change behind it is sent once.
    #[test]
    fn resuming_a_journal_sends_each_change_exactly_once() {
        let mut log = Log::new(0);
        // Someone else committed seq 1 and 2 while we were away; we started from seq 0.
        for (i, t) in ["x", "y"].iter().enumerate() {
            assert!(matches!(log.commit(i as u64, "other", i as u64 + 1, "bob", vec![put(5 + i as u32, t)]), Ok(Commit::New { .. })));
        }
        // Our client was at seq 2 with one batch in flight and one change buffered, then died.
        let mut s = Session::new("mine", 2);
        let sent = s.local(vec![put(0, "first")]).expect("sent at once");
        assert!(s.local(vec![put(1, "second")]).is_none());
        // The server committed the first batch (seq 3); the ack was lost with the program.
        assert!(matches!(log.commit(sent.base, "mine", sent.id, "ann", sent.ops.clone()), Ok(Commit::New { seq: 3, .. })));
        let mut j = Journal::new("test", "u", "ann", "i", "n", s.state());
        let d = dir("once");
        j.write(&d).unwrap();
        drop(s);

        // Next start: the file is the version at seq 0; the journal says its changes are at seq 2.
        let j = Journal::find(&d, "u", "ann", "i").unwrap();
        let mut s = Session::new(j.session.cid.clone(), 0);
        s.online = false;
        let mut applied = 0;
        let mut restored = false;
        for e in log.since(0).unwrap().to_vec() {
            if !restored && s.seq == j.session.seq {
                assert_eq!(s.restore(&j.session).len(), 2);
                restored = true;
            }
            let st = s.remote_based(e.seq, &e.cid, e.id, e.base, e.ops);
            applied += st.apply.len();
        }
        assert!(restored);
        // Seq 3 was our own batch, so only the buffered change is still unacknowledged.
        assert_eq!(applied, 2, "only the two changes of others before the journal's sequence number");
        assert!(s.inflight.is_none() && s.buffer.len() == 1);
        let b = s.resume().expect("the buffered change goes out");
        assert_eq!((b.base, b.ops.len()), (3, 1));
        assert!(b.id > sent.id, "batch ids carry on");
        assert!(matches!(log.commit(b.base, "mine", b.id, "ann", b.ops), Ok(Commit::New { seq: 4, .. })));
        let _ = std::fs::remove_dir_all(&d);
    }
}
