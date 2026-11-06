//! The client side of the protocol: one batch in flight, later local changes buffered,
//! remote batches transformed over both before they are applied locally.

use crate::{transform, Op};

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
    /// False while reconnecting: local changes are only buffered.
    pub online: bool,
}

impl Session {
    pub fn new(cid: impl Into<String>, seq: u64) -> Self {
        Session { cid: cid.into(), seq, inflight: None, buffer: Vec::new(), next_id: 1, online: true }
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
        let mut apply: Vec<Op> = extra.into_iter().map(|(_, o)| o).collect();
        if !apply.is_empty() && !self.buffer.is_empty() {
            let (r, mine) = transform(&apply, &self.buffer, false);
            apply = r;
            self.buffer = mine;
        }
        Step { apply, send: self.flush(), own: true, committed }
    }

    /// A committed batch from the server.
    pub fn remote(&mut self, seq: u64, cid: &str, id: u64, ops: Vec<Op>) -> Step {
        if cid == self.cid && self.inflight.as_ref().is_some_and(|b| b.id == id) {
            // Replayed after a reconnect: the committed form may contain server additions.
            let extra: Vec<Op> = ops.iter().filter(|o| matches!(o, Op::AddSheet { s, .. } if *s == crate::RESCUE_SHEET)).cloned().collect();
            let mut st = self.ack(id, seq, Vec::new());
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
        Step { apply: ops, send: None, own: false, committed }
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
