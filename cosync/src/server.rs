//! The server side of the protocol: the committed log of one document.

use std::collections::BTreeSet;

use crate::{transform, Op, SheetId, RESCUE_SHEET};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub seq: u64,
    pub cid: String,
    pub id: u64,
    /// Who made it (server-defined, e.g. a user id).
    pub by: String,
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
}

impl Log {
    pub fn new(floor: u64) -> Self {
        Log { floor, entries: Vec::new(), sheets: None }
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
        if let Some(e) = self.entries.iter().find(|e| e.cid == cid && e.id == id) {
            return Ok(Commit::Duplicate { seq: e.seq });
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
        self.entries.push(Entry { seq, cid: cid.to_string(), id, by: by.to_string(), ops: ops.clone() });
        Ok(Commit::New { seq, ops, extra })
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
