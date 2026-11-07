//! The server side of the protocol: the committed log of one document.

use std::collections::{BTreeMap, BTreeSet};

use crate::{transform, Op, SheetId, RESCUE_SHEET};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub seq: u64,
    pub cid: String,
    pub id: u64,
    /// Who made it (server-defined, e.g. a user id).
    pub by: String,
    /// The sequence number the sender had seen (None when loaded from storage).
    pub base: Option<u64>,
    pub ops: Vec<Op>,
}

#[derive(Debug, PartialEq)]
pub enum Commit {
    /// Committed as `seq` in this (transformed) form. `extra` are ops the server added
    /// (already part of `ops`, at these indices) that the sender must apply too.
    New { seq: u64, ops: Vec<Op>, extra: Vec<(u32, Op)> },
    /// Already committed earlier as `seq` (a resend after a reconnect).
    Duplicate { seq: u64 },
}

#[derive(Debug, PartialEq)]
pub enum CommitError {
    /// The base is older than the oldest kept entry: the client must reload.
    TooOld,
    /// The base is in the future.
    Invalid,
}

/// Committed batches `floor+1..=head`.
#[derive(Clone, Debug, Default)]
pub struct Log {
    pub floor: u64,
    pub entries: Vec<Entry>,
    /// The document's sheets at `head`, when known. Used to give the workbook a sheet again
    /// when concurrent deletes removed them all.
    pub sheets: Option<BTreeSet<SheetId>>,
    /// Each client's last committed batch (id, seq) by (who, cid), kept when its entry is
    /// pruned: so a client whose ack was lost learns its batch is in even after it reloads.
    pub last: BTreeMap<(String, String), (u64, u64)>,
}

impl Log {
    pub fn new(floor: u64) -> Self {
        Log { floor, entries: Vec::new(), sheets: None, last: BTreeMap::new() }
    }

    /// Sheets of the state at `head` (from a snapshot, or the first client).
    pub fn set_sheets(&mut self, ids: impl IntoIterator<Item = SheetId>) {
        let set: BTreeSet<SheetId> = ids.into_iter().collect();
        self.sheets = (!set.is_empty()).then_some(set);
    }

    pub fn head(&self) -> u64 {
        self.entries.last().map_or(self.floor, |e| e.seq)
    }

    /// Entries after `seq`, or None when they are no longer kept.
    pub fn since(&self, seq: u64) -> Option<&[Entry]> {
        if seq < self.floor || seq > self.head() {
            return None;
        }
        let i = self.entries.partition_point(|e| e.seq <= seq);
        Some(&self.entries[i..])
    }

    /// Transforms a client batch over everything committed after its base and commits it.
    pub fn commit(&mut self, base: u64, cid: &str, id: u64, by: &str, ops: Vec<Op>) -> Result<Commit, CommitError> {
        self.commit_made(base, base, cid, id, by, ops)
    }

    /// Like `commit`; `made` (<= `base`) is what the sender had seen when it made the changes,
    /// which is what other clients are told to tell concurrent edits.
    pub fn commit_made(&mut self, base: u64, made: u64, cid: &str, id: u64, by: &str, ops: Vec<Op>) -> Result<Commit, CommitError> {
        if let Some(e) = self.entries.iter().find(|e| e.cid == cid && e.id == id && e.by == by) {
            return Ok(Commit::Duplicate { seq: e.seq });
        }
        if let Some((_, seq)) = self.last_commit(cid, by).filter(|l| l.0 == id) {
            return Ok(Commit::Duplicate { seq });
        }
        let later = self.since(base).ok_or(if base > self.head() { CommitError::Invalid } else { CommitError::TooOld })?;
        let mut ops = ops;
        for e in later {
            ops = transform(&ops, &e.ops, true).0;
        }
        let mut extra = Vec::new();
        if let Some(set) = &mut self.sheets {
            // A batch that deletes the last sheet gets the rescue sheet right after that delete,
            // so the rest of the batch (and everyone else) finds a sheet.
            let mut out = Vec::with_capacity(ops.len());
            for op in ops {
                let emptied = match &op {
                    Op::AddSheet { s, .. } => {
                        set.insert(*s);
                        false
                    }
                    Op::DelSheet { s, .. } => set.remove(s) && set.is_empty(),
                    _ => false,
                };
                out.push(op);
                if emptied {
                    let rescue = Op::AddSheet { s: RESCUE_SHEET, name: "Sheet1".into(), pos: 0.0 };
                    set.insert(RESCUE_SHEET);
                    extra.push((out.len() as u32, rescue.clone()));
                    out.push(rescue);
                }
            }
            ops = out;
        }
        let seq = self.head() + 1;
        self.entries.push(Entry { seq, cid: cid.to_string(), id, by: by.to_string(), base: Some(made.min(base)), ops: ops.clone() });
        self.last.insert((by.to_string(), cid.to_string()), (id, seq));
        Ok(Commit::New { seq, ops, extra })
    }

    /// The last batch `cid` (of `by`) committed (id, seq), if any.
    pub fn last_commit(&self, cid: &str, by: &str) -> Option<(u64, u64)> {
        self.last.get(&(by.to_string(), cid.to_string())).copied()
    }

    /// Forgets entries up to and including `seq`.
    pub fn prune_to(&mut self, seq: u64) {
        let seq = seq.min(self.head());
        if seq <= self.floor {
            return;
        }
        self.entries.retain(|e| e.seq > seq);
        self.floor = seq;
    }
}
