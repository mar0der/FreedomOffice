//! `fsheets mcp`: a Model Context Protocol server over standard input and output (newline
//! delimited JSON-RPC 2.0, protocol revision 2025-06-18, older ones accepted). There is no
//! network listener: the agent's host starts this process and talks to it through pipes.
//! Standard output carries protocol messages only; diagnostics go to standard error and never
//! contain passwords or tokens.
//!
//! Sessions: `join_session` keeps a live connection (the agent shows in the presence of the
//! people editing, with the range it is working on); reads and writes go through it, so people
//! see edits as they happen. Without a joined session each call joins for the moment it takes
//! (so it sees what was edited live), and a write saves a snapshot (a version) and leaves.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};

use crate::api::Api;
use crate::live::Live;
use crate::work;

const LATEST: &str = "2025-06-18";
const SUPPORTED: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "FreedomOffice Sheets. Find a workbook with list_files, then give its id as `item` to the other tools (after open_file or join_session the item may be left out). read_range returns calculated values and formula text; write_range takes a 2D array whose strings starting with = are formulas. Edits made while joined (join_session) are seen live by the people who have the workbook open and show this agent in their presence; leave_session when done so the work is saved as a version.";

pub struct Mcp {
    api: Api,
    sessions: BTreeMap<String, Live>,
    current: Option<String>,
    last_reconnect: Instant,
}

fn text_of(v: &Json) -> String {
    v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string())
}

fn str_arg<'a>(a: &'a Json, k: &str) -> Option<&'a str> {
    a[k].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn need_str<'a>(a: &'a Json, k: &str) -> Result<&'a str, String> {
    str_arg(a, k).ok_or_else(|| format!("`{k}` is required."))
}

/// The grid of a write: an array of rows, a single row, or a single value.
fn grid_of(v: &Json) -> Result<Vec<Vec<Json>>, String> {
    match v {
        Json::Array(rows) if rows.iter().all(Json::is_array) && !rows.is_empty() => Ok(rows
            .iter()
            .map(|r| r.as_array().cloned().unwrap_or_default())
            .collect()),
        Json::Array(row) => Ok(vec![row.clone()]),
        Json::Null => Err("`values` is required: a 2D array like [[1,2],[\"=A1+B1\",3]].".into()),
        one => Ok(vec![vec![one.clone()]]),
    }
}

impl Mcp {
    pub fn new(api: Api) -> Mcp {
        Mcp {
            api,
            sessions: BTreeMap::new(),
            current: None,
            last_reconnect: Instant::now(),
        }
    }

    /// Keeps joined sessions alive: receives what others did, answers snapshot requests,
    /// reconnects after a lost connection.
    pub fn tick(&mut self) {
        let retry = self.last_reconnect.elapsed() > Duration::from_secs(2);
        for (item, l) in self.sessions.iter_mut() {
            l.pump(Duration::from_millis(1));
            if !l.connected()
                && l.stopped.is_none()
                && retry
                && let Err(e) = l.connect()
            {
                eprintln!("fsheets: reconnecting to {item}: {e}");
            }
        }
        if retry {
            self.last_reconnect = Instant::now();
        }
        let gone: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, l)| l.stopped.is_some())
            .map(|(i, _)| i.clone())
            .collect();
        for i in gone {
            if let Some(l) = self.sessions.remove(&i) {
                eprintln!(
                    "fsheets: session on {i} ended: {}",
                    l.stopped.clone().unwrap_or_default()
                );
            }
        }
    }

    fn item(&self, a: &Json) -> Result<String, String> {
        str_arg(a, "item")
            .map(str::to_string)
            .or_else(|| self.current.clone())
            .ok_or_else(|| "`item` is required (an id from list_files); open_file or join_session sets a default.".into())
    }

    /// Runs `f` on the joined session, or on a session joined for the moment it takes. A write
    /// without a joined session saves a snapshot (a version) before it leaves.
    fn with_live<T>(
        &mut self,
        item: &str,
        write: bool,
        f: impl FnOnce(&mut Live) -> Result<T, String>,
    ) -> Result<(T, Option<i64>), String> {
        self.tick();
        if let Some(l) = self.sessions.get_mut(item) {
            if !l.connected() {
                l.connect()?;
                l.wait_synced(Duration::from_secs(10));
            }
            let out = f(l)?;
            if write && !l.wait_synced(Duration::from_secs(20)) {
                return Err("The server didn't confirm the change yet; it will be sent when the connection is back.".into());
            }
            return Ok((out, None));
        }
        // The saved file lacks what was edited live since; joining replays it.
        let mut l = Live::open(&self.api, item, true)?;
        if !l.wait_synced(Duration::from_secs(20)) {
            return Err(l
                .stopped
                .clone()
                .unwrap_or_else(|| "Couldn't join the live session.".into()));
        }
        let out = f(&mut l);
        let version = if write && out.is_ok() {
            work::settle(&mut l)
        } else {
            Ok(None)
        };
        l.disconnect();
        Ok((out?, version?))
    }

    pub fn call(&mut self, name: &str, a: &Json) -> Result<Json, String> {
        match name {
            "list_files" => work::list_files(&self.api, str_arg(a, "folder")),
            "create_file" => {
                let sheets: Vec<String> = a["sheets"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(|s| s.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let it = work::create_file(
                    &self.api,
                    need_str(a, "folder")?,
                    need_str(a, "name")?,
                    &sheets,
                )?;
                self.current = Some(it.id.clone());
                Ok(work::item_json(&it))
            }
            "open_file" => {
                let item = need_str(a, "item")?.to_string();
                let meta = self.api.item(&item)?;
                if meta.folder {
                    return Err("That is a folder; open a file (list_files with folder).".into());
                }
                let (sheets, _) = self.with_live(&item, false, |l| Ok(work::list_sheets(l)))?;
                self.current = Some(item.clone());
                Ok(
                    json!({ "file": work::item_json(&meta), "joined": self.sessions.contains_key(&item), "sheets": sheets["sheets"] }),
                )
            }
            "list_sheets" => {
                let item = self.item(a)?;
                Ok(self
                    .with_live(&item, false, |l| Ok(work::list_sheets(l)))?
                    .0)
            }
            "read_range" => {
                let item = self.item(a)?;
                let (sheet, range) = (str_arg(a, "sheet"), str_arg(a, "range"));
                let display = a["display"].as_bool().unwrap_or(false);
                Ok(self
                    .with_live(&item, false, |l| work::read_range(l, sheet, range, display))?
                    .0)
            }
            "write_range" => {
                let item = self.item(a)?;
                let grid = grid_of(&a["values"])?;
                let (sheet, range) = (str_arg(a, "sheet"), need_str(a, "range")?);
                let (mut out, version) =
                    self.with_live(&item, true, |l| work::write_range(l, sheet, range, &grid))?;
                self.note_saved(&item, &mut out, version);
                Ok(out)
            }
            "add_sheet" => {
                let item = self.item(a)?;
                let name = need_str(a, "name")?;
                let (mut out, version) =
                    self.with_live(&item, true, |l| work::add_sheet(l, name))?;
                self.note_saved(&item, &mut out, version);
                Ok(out)
            }
            "evaluate" => {
                let item = self.item(a)?;
                let (sheet, src) = (str_arg(a, "sheet"), need_str(a, "formula")?);
                Ok(self
                    .with_live(&item, false, |l| work::evaluate(l, sheet, src))?
                    .0)
            }
            "export" => {
                let item = self.item(a)?;
                let path = PathBuf::from(need_str(a, "path")?);
                let (sheet, over) = (
                    str_arg(a, "sheet"),
                    a["overwrite"].as_bool().unwrap_or(false),
                );
                Ok(self
                    .with_live(&item, false, |l| work::export(l, &path, sheet, over))?
                    .0)
            }
            "list_versions" => work::versions(&self.api, &self.item(a)?),
            "join_session" => {
                let item = self.item(a)?;
                self.current = Some(item.clone());
                if !self.sessions.contains_key(&item) {
                    let mut l = Live::open(&self.api, &item, true)?;
                    if !l.wait_synced(Duration::from_secs(20)) {
                        return Err(l
                            .stopped
                            .clone()
                            .unwrap_or_else(|| "Couldn't join the live session.".into()));
                    }
                    self.sessions.insert(item.clone(), l);
                }
                let l = self.sessions.get_mut(&item).expect("just joined");
                match str_arg(a, "range") {
                    Some(r) => {
                        let p = crate::a1::parse_ref(r)?;
                        let idx =
                            work::find_sheet(&l.wb, p.sheet.as_deref().or(str_arg(a, "sheet")))?;
                        let uid = l.wb.sheets[idx].uid;
                        work::show_presence(l, uid, p.range, false);
                    }
                    None => {
                        if let Some(first) = l.first_sheet() {
                            work::show_presence(
                                l,
                                first,
                                freedom_sheets::model::Range::cell(0, 0),
                                false,
                            );
                        }
                    }
                }
                l.pump(Duration::from_millis(100));
                let mut out = work::peers(l);
                out["joined"] = json!(item);
                out["sheets"] = work::list_sheets(l)["sheets"].clone();
                Ok(out)
            }
            "leave_session" => {
                let item = self.item(a)?;
                let mut l = self
                    .sessions
                    .remove(&item)
                    .ok_or("There is no joined session for that item.")?;
                let saved = work::settle(&mut l);
                l.disconnect();
                let version = saved?;
                Ok(json!({ "left": item, "saved_version": version, "unsaved_changes": false }))
            }
            "who_is_here" => {
                let item = self.item(a)?;
                self.tick();
                if let Some(l) = self.sessions.get_mut(&item) {
                    l.pump(Duration::from_millis(50));
                    return Ok(work::peers(l));
                }
                // Look in without staying: join, see who is there, leave without changing anything.
                let mut l = Live::open(&self.api, &item, true)?;
                l.wait_synced(Duration::from_secs(15));
                l.pump(Duration::from_millis(300));
                let out = work::peers(&l);
                l.disconnect();
                Ok(out)
            }
            other => Err(format!("Unknown tool '{other}'.")),
        }
    }

    fn note_saved(&mut self, item: &str, out: &mut Json, version: Option<i64>) {
        let joined = self.sessions.contains_key(item);
        out["session"] = json!(if joined {
            "joined: seen live by others; leave_session saves a version"
        } else {
            "one-off: joined, wrote, saved a version, left"
        });
        if let Some(v) = version {
            out["saved_version"] = json!(v);
        }
        self.current = Some(item.to_string());
    }

    /// Leaves every session cleanly (snapshot included).
    pub fn shutdown(&mut self) {
        for (item, mut l) in std::mem::take(&mut self.sessions) {
            if let Err(e) = work::settle(&mut l) {
                eprintln!("fsheets: leaving {item}: {e}");
            }
            l.disconnect();
        }
    }

    /// One JSON-RPC message. Returns the response, or None for notifications.
    pub fn handle(&mut self, msg: &Json) -> Option<Json> {
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or("");
        let reply = |result: Json| {
            id.clone()
                .map(|id| json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        };
        let fail = |code: i64, text: &str| json!({ "jsonrpc": "2.0", "id": id.clone().unwrap_or(Json::Null), "error": { "code": code, "message": text } });
        if !msg.is_object() || msg["jsonrpc"] != "2.0" || method.is_empty() {
            // A response from the client (to a request we never send) or garbage.
            return if msg.get("result").is_some() || msg.get("error").is_some() {
                None
            } else {
                Some(fail(-32600, "Invalid request"))
            };
        }
        match method {
            "initialize" => {
                let want = msg["params"]["protocolVersion"].as_str().unwrap_or(LATEST);
                let version = SUPPORTED
                    .iter()
                    .find(|v| **v == want)
                    .copied()
                    .unwrap_or(LATEST);
                reply(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "fsheets", "title": "FreedomOffice Sheets", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => reply(json!({})),
            "tools/list" => reply(json!({ "tools": tools() })),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                if !tools()
                    .as_array()
                    .is_some_and(|t| t.iter().any(|x| x["name"] == name))
                {
                    return id
                        .as_ref()
                        .map(|_| fail(-32602, &format!("Unknown tool: {name}")));
                }
                let args = match &msg["params"]["arguments"] {
                    Json::Null => json!({}),
                    a => a.clone(),
                };
                let result = match self.call(name, &args) {
                    Ok(v) => {
                        json!({ "content": [{ "type": "text", "text": text_of(&v) }], "isError": false })
                    }
                    Err(e) => {
                        json!({ "content": [{ "type": "text", "text": e }], "isError": true })
                    }
                };
                reply(result)
            }
            m if m.starts_with("notifications/") => None,
            _ => id
                .as_ref()
                .map(|_| fail(-32601, &format!("Method not found: {method}"))),
        }
    }
}

/// Serves requests from `input` until it ends, writing responses to `out`. While waiting for
/// input the joined sessions are kept alive.
pub fn serve(
    api: Api,
    input: impl BufRead + Send + 'static,
    mut out: impl Write,
) -> Result<(), String> {
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        for line in input.lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    let mut mcp = Mcp::new(api);
    let mut send = |v: &Json| -> Result<(), String> {
        writeln!(out, "{v}")
            .and_then(|_| out.flush())
            .map_err(|e| e.to_string())
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                let resp = match serde_json::from_str::<Json>(&line) {
                    Ok(Json::Array(_)) => Some(
                        json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "Batches are not supported" } }),
                    ),
                    Ok(msg) => mcp.handle(&msg),
                    Err(_) => Some(
                        json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }),
                    ),
                };
                if let Some(r) = resp {
                    send(&r)?;
                }
            }
            Err(RecvTimeoutError::Timeout) => mcp.tick(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    mcp.shutdown();
    Ok(())
}

fn tool(name: &str, description: &str, props: Json, required: &[&str]) -> Json {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required },
    })
}

fn tools() -> Json {
    let item = json!({ "type": "string", "description": "File id from list_files. May be left out after open_file / join_session / create_file." });
    let sheet = json!({ "type": "string", "description": "Sheet name. Defaults to the first sheet (or the sheet named in the range, like Data!A1:B2)." });
    json!([
        tool(
            "list_files",
            "Without `folder`: where to start: My files (folder id), organisation folders, and files shared with you. With `folder`: the files and folders inside it.",
            json!({ "folder": { "type": "string", "description": "Folder id." } }),
            &[]
        ),
        tool(
            "open_file",
            "Opens a workbook (does not join the live session) and lists its sheets with their used ranges. Makes it the default item.",
            json!({ "item": item }),
            &["item"]
        ),
        tool(
            "create_file",
            "Creates a new empty workbook (.xlsx) in a folder and makes it the default item.",
            json!({ "folder": { "type": "string", "description": "Folder id." }, "name": { "type": "string" }, "sheets": { "type": "array", "items": { "type": "string" }, "description": "Sheet names; default one sheet." } }),
            &["folder", "name"]
        ),
        tool(
            "list_sheets",
            "Sheets of a workbook with used ranges.",
            json!({ "item": item }),
            &[]
        ),
        tool(
            "read_range",
            "Reads cells. Returns calculated `values` (null = empty, errors as text like #DIV/0!) and `formulas` (the =formula text or null) as 2D arrays. Without `range` the whole used range. While joined it shows the range as this agent's selection.",
            json!({ "item": item, "sheet": sheet, "range": { "type": "string", "description": "A1 notation: B7, A1:C5, A:C, 3:5, Data!A1:B2." }, "display": { "type": "boolean", "description": "Also return the text as formatted in the workbook." } }),
            &[]
        ),
        tool(
            "write_range",
            "Writes a 2D array starting at the top-left cell of `range` (a single cell, or a range of exactly the array's size). Strings starting with = are formulas, other strings are read as typed in a cell (\"12\" is a number, \"'12\" text), numbers and booleans as given, null clears. Cell formats are kept. Returns the calculated values. Nothing is written if any entry is invalid. Goes through the joined live session when there is one, otherwise joins briefly and saves a version.",
            json!({ "item": item, "sheet": sheet, "range": { "type": "string" }, "values": { "type": "array", "items": { "type": "array" }, "description": "Rows of cells, e.g. [[1,2],[\"=A1+B1\",null]]." } }),
            &["range", "values"]
        ),
        tool(
            "add_sheet",
            "Adds a sheet at the end.",
            json!({ "item": item, "name": { "type": "string" } }),
            &["name"]
        ),
        tool(
            "evaluate",
            "Calculates a formula against the workbook (as if typed in A1) with FreedomSheets' own engine, without changing anything.",
            json!({ "item": item, "sheet": sheet, "formula": { "type": "string", "description": "e.g. =SUM(Budget!B2:B9)/COUNT(B2:B9)" } }),
            &["formula"]
        ),
        tool(
            "export",
            "Writes the workbook to a local file: .xlsx (all sheets) or .csv/.tsv (one sheet, as formatted). Refuses to replace an existing file unless overwrite is true.",
            json!({ "item": item, "sheet": sheet, "path": { "type": "string", "description": "Local file path ending in .xlsx, .csv or .tsv." }, "overwrite": { "type": "boolean" } }),
            &["path"]
        ),
        tool(
            "list_versions",
            "The version history of a file.",
            json!({ "item": item }),
            &[]
        ),
        tool(
            "join_session",
            "Joins the workbook's live session and stays connected, marked as an agent in the presence of the people editing. Edits then appear live for them. Optionally shows `range` as this agent's selection.",
            json!({ "item": item, "sheet": sheet, "range": { "type": "string" } }),
            &[]
        ),
        tool(
            "leave_session",
            "Leaves the live session. Changes are sent first and saved as a version.",
            json!({ "item": item }),
            &[]
        ),
        tool(
            "who_is_here",
            "Who has the workbook open, and the range each is working on. Works without a joined session.",
            json!({ "item": item }),
            &[]
        ),
    ])
}
