//! The client side of the protocol: one batch in flight, later local changes buffered,
//! remote batches transformed over both before they are applied locally.

use std::collections::{HashMap, VecDeque};

use crate::transform::write_key;
use crate::{cell_write, transform, CellWrite, Op, SheetId};

/// How many of our own acknowledged batches are remembered to tell when one was replaced.
const RECENT_BATCHES: usize = 32;
/// ...and how many cell writes at most (a huge paste is not tracked).
const RECENT_WRITES: usize = 20_000;

/// One of our own cell edits that someone else's concurrent edit replaced: we both wrote the
/// same part of the cell without seeing each other's change, and theirs was committed last.
#[derive(Clone, Debug, PartialEq)]
pub struct Replaced {
    pub s: SheetId,
    pub r: u32,
    pub c: u32,
    pub part: CellWrite,
    /// What we wrote, as a `Content` or `Format` operation (apply it again to take it back).
    pub mine: Op,
    /// What replaced it.
    pub theirs: Op,
}

/// A batch to send to the server.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    pub id: u64,
    pub base: u64,
    pub ops: Vec<Op>,
}

/// What the app must do after an event.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    /// Remote changes to apply to the local workbook (already transformed).
    pub apply: Vec<Op>,
    /// A batch to send now.
    pub send: Option<Batch>,
    /// The event concerned this client's own batch (an ack, or its replay after a reconnect).
    pub own: bool,
    /// The batch exactly as the server committed it (remote batches, and our own on ack):
    /// applying these in order reproduces the server's state.
    pub committed: Vec<Op>,
    /// Our own edits this remote batch replaced (it was made without seeing them).
    pub replaced: Vec<Replaced>,
}

impl Step {
    fn with(mut self, replaced: Vec<Replaced>) -> Step {
        self.replaced = replaced;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    /// This app instance (identifies its batches in replays).
    pub cid: String,
    /// Last server sequence number applied locally.
    pub seq: u64,
    pub inflight: Option<Batch>,
    pub buffer: Vec<Op>,
    next_id: u64,
    /// Cell writes of our recently acknowledged batches (sequence number, single-part
    /// operations), kept in the coordinates of the latest state.
    recent: VecDeque<(u64, Vec<Op>)>,
    /// False while reconnecting: local changes are only buffered.
    pub online: bool,
}

impl Session {
    pub fn new(cid: impl Into<String>, seq: u64) -> Self {
        Session { cid: cid.into(), seq, inflight: None, buffer: Vec::new(), next_id: 1, recent: VecDeque::new(), online: true }
    }

    /// Unacknowledged local changes, oldest first.
    pub fn pending(&self) -> Vec<Op> {
        let mut v = self.inflight.as_ref().map(|b| b.ops.clone()).unwrap_or_default();
        v.extend(self.buffer.iter().cloned());
        v
    }

    pub fn has_pending(&self) -> bool {
        self.inflight.is_some() || !self.buffer.is_empty()
    }

    fn flush(&mut self) -> Option<Batch> {
        if !self.online || self.inflight.is_some() || self.buffer.is_empty() {
            return None;
        }
        let b = Batch { id: self.next_id, base: self.seq, ops: std::mem::take(&mut self.buffer) };
        self.next_id += 1;
        self.inflight = Some(b.clone());
        Some(b)
    }

    /// Local changes (already applied to the local workbook).
    pub fn local(&mut self, ops: Vec<Op>) -> Option<Batch> {
        self.buffer.extend(ops);
        self.flush()
    }

    /// The server committed our in-flight batch as `seq`, inserting `extra` (to apply).
    pub fn ack(&mut self, id: u64, seq: u64, extra: Vec<(u32, Op)>) -> Step {
        let Some(b) = self.inflight.take_if(|b| b.id == id) else { return Step::default() };
        self.seq = seq;
        let mut committed = b.ops;
        for (i, op) in &extra {
            committed.insert((*i as usize).min(committed.len()), op.clone());
        }
        self.record(seq, &committed);
        let mut apply: Vec<Op> = extra.into_iter().map(|(_, o)| o).collect();
        if !apply.is_empty() && !self.buffer.is_empty() {
            let (r, mine) = transform(&apply, &self.buffer, false);
            apply = r;
            self.buffer = mine;
        }
        Step { apply, send: self.flush(), own: true, committed, replaced: Vec::new() }
    }

    /// Remembers the cell writes of our batch committed as `seq`.
    fn record(&mut self, seq: u64, committed: &[Op]) {
        let writes: Vec<Op> = committed.iter().flat_map(|o| cell_write(o).into_iter().map(|(_, o)| o)).collect();
        if writes.is_empty() || writes.len() > RECENT_WRITES {
            return;
        }
        self.recent.push_back((seq, writes));
        while self.recent.len() > RECENT_BATCHES || self.recent.iter().map(|(_, w)| w.len()).sum::<usize>() > RECENT_WRITES {
            self.recent.pop_front();
        }
    }

    /// Our own edits that the remote batch `ops` (committed as `seq`, made when its sender had
    /// seen up to `base`) replaced; and moves the remembered writes along with `ops`.
    fn replaced_by(&mut self, base: Option<u64>, ops: &[Op]) -> Vec<Replaced> {
        let mut out = Vec::new();
        if self.recent.is_empty() {
            return out;
        }
        if let Some(base) = base {
            let theirs: Vec<Op> = ops.iter().flat_map(|o| cell_write(o).into_iter().map(|(_, o)| o)).collect();
            if !theirs.is_empty() {
                // What we have written since and not had acknowledged stays ours: no notice.
                let pending: Vec<Op> = self.pending().iter().flat_map(|o| cell_write(o).into_iter().map(|(_, o)| o)).collect();
                let key = |o: &Op| write_key(o).map(|k| (k.0, k.1, k.2, k.3));
                let again: std::collections::HashSet<_> = pending.iter().filter_map(key).collect();
                let theirs_by: HashMap<_, &Op> = theirs.iter().filter_map(|o| key(o).map(|k| (k, o))).collect();
                for (seq, writes) in &self.recent {
                    if *seq <= base {
                        continue;
                    }
                    for mine in writes {
                        let Some(k) = key(mine) else { continue };
                        let Some(t) = theirs_by.get(&k) else { continue };
                        if again.contains(&k) || write_key(mine) == write_key(t) {
                            continue;
                        }
                        out.push(Replaced { s: k.1, r: k.2, c: k.3, part: k.0, mine: mine.clone(), theirs: (*t).clone() });
                    }
                }
            }
        }
        // Follow the structure changes in `ops`; what it overwrote is not remembered again.
        for (_, writes) in &mut self.recent {
            *writes = transform(writes, ops, false).0;
        }
        self.recent.retain(|(_, w)| !w.is_empty());
        out
    }

    /// A committed batch from the server.
    pub fn remote(&mut self, seq: u64, cid: &str, id: u64, ops: Vec<Op>) -> Step {
        self.remote_based(seq, cid, id, None, ops)
    }

    /// A committed batch from the server, with the `base` it was sent against when the server
    /// says (see `ServerMsg::Ops`): needed to tell edits it replaced.
    pub fn remote_based(&mut self, seq: u64, cid: &str, id: u64, base: Option<u64>, ops: Vec<Op>) -> Step {
        if cid == self.cid && self.inflight.as_ref().is_some_and(|b| b.id == id) {
            // Replayed after a reconnect: the committed form may contain server additions.
            let extra: Vec<Op> = ops.iter().filter(|o| matches!(o, Op::AddSheet { s, .. } if *s == crate::RESCUE_SHEET)).cloned().collect();
            let mut st = self.ack(id, seq, Vec::new());
            self.recent.pop_back();
            self.record(seq, &ops);
            st.committed = ops;
            if !extra.is_empty() {
                let (r, mine) = transform(&extra, &self.buffer, false);
                self.buffer = mine;
                st.apply = r;
            }
            return st;
        }
        self.seq = seq;
        let committed = ops.clone();
        let replaced = self.replaced_by(base, &committed);
        let mut ops = ops;
        if let Some(b) = &mut self.inflight {
            let (r, mine) = transform(&ops, &b.ops, false);
            ops = r;
            b.ops = mine;
        }
        if !self.buffer.is_empty() {
            let (r, mine) = transform(&ops, &self.buffer, false);
            ops = r;
            self.buffer = mine;
        }
        Step { apply: ops, send: None, own: false, committed, replaced: Vec::new() }.with(replaced)
    }

    /// Connection lost: keep editing, buffer everything.
    pub fn offline(&mut self) {
        self.online = false;
    }

    /// Reconnected and caught up with the server (all replayed batches applied): resend the
    /// in-flight batch, now based on the current sequence number.
    pub fn resume(&mut self) -> Option<Batch> {
        self.online = true;
        if let Some(b) = &mut self.inflight {
            b.base = self.seq;
            return Some(b.clone());
        }
        self.flush()
    }

    /// After reloading the latest snapshot (the server no longer had our base): everything
    /// pending becomes one new batch against `seq`. Returns the ops to re-apply locally.
    pub fn rebase_on_reload(&mut self, seq: u64) -> Vec<Op> {
        let ops = self.pending();
        self.inflight = None;
        self.buffer = ops.clone();
        self.seq = seq;
        ops
    }
}
