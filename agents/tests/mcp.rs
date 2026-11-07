//! `fsheets mcp` driven over pipes by a minimal MCP client, against a real server started
//! in-process (127.0.0.1, random port, temp data). A second, headless client plays the person.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{Running, spawn};
use fsheets::api::Api;
use fsheets::live::Live;
use fsheets::sample;
use serde_json::{Value, json};

/// Test-only password for the throwaway accounts.
const PW: &str = "test-password-1";

struct Srv {
    run: Option<Running>,
    dir: std::path::PathBuf,
    url: String,
}

impl Drop for Srv {
    fn drop(&mut self) {
        drop(self.run.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start() -> Srv {
    let dir = std::env::temp_dir().join(format!(
        "fo-mcp-test-{}",
        freedomoffice_server::util::new_id()
    ));
    let cfg = Config {
        data_dir: dir.join("data"),
        login_delay_ms: 10,
        ..Config::default()
    };
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        for (u, admin) in [("alice", true), ("agent", false)] {
            s.create_user(u, u, "", PW, admin).unwrap();
        }
    }
    let run = spawn(cfg).unwrap();
    let url = run.url();
    Srv {
        run: Some(run),
        dir,
        url,
    }
}

/// A minimal MCP client over the child's pipes.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next: u64,
}

impl Client {
    fn start(srv: &Srv, dir: &std::path::Path) -> Client {
        let pw = dir.join("agent.pw");
        std::fs::write(&pw, format!("{PW}\n")).unwrap();
        std::fs::set_permissions(&pw, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_fsheets"))
            .args([
                "mcp",
                "--server",
                &srv.url,
                "--user",
                "agent",
                "--password-file",
            ])
            .arg(&pw)
            .arg("--token-file")
            .arg(dir.join("agent.token"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Client {
            child,
            stdin,
            lines,
            next: 1,
        }
    }

    fn send(&mut self, v: &Value) {
        let w = self.stdin.as_mut().unwrap();
        writeln!(w, "{v}").unwrap();
        w.flush().unwrap();
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(60))
            .expect("no answer from fsheets mcp");
        let v: Value = serde_json::from_str(&line).expect("stdout carries JSON only");
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], id);
        v
    }

    /// A tool call: Ok(parsed JSON of the text) or Err(error text).
    fn tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let r = self.rpc("tools/call", json!({ "name": name, "arguments": args }));
        let res = &r["result"];
        assert!(res.is_object(), "{r}");
        let text = res["content"][0]["text"].as_str().unwrap().to_string();
        if res["isError"] == true {
            Err(text)
        } else {
            Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
        }
    }

    fn close(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        self.child.wait().unwrap()
    }
}

fn versions(api: &Api, item: &str) -> usize {
    api.versions(item).unwrap().len()
}

#[test]
fn an_agent_edits_through_mcp_and_people_see_it() {
    let srv = start();
    let dir = srv.dir.join("work");
    std::fs::create_dir_all(&dir).unwrap();

    // Alice owns Budget.xlsx and shares it with the agent.
    let alice = Api::login(&srv.url, "alice", PW, "test").unwrap();
    let root = alice.root().unwrap();
    let item = alice
        .upload_new(&root, "Budget.xlsx", sample::make("budget").unwrap())
        .unwrap()
        .id;
    let r = alice
        .send(
            "POST",
            &format!("/api/items/{item}/permissions"),
            &[("Content-Type", "application/json")],
            Some(br#"{"username":"agent","role":"edit"}"#.to_vec()),
        )
        .unwrap();
    assert!(r.ok(), "{}", r.status);

    let mut c = Client::start(&srv, &dir);

    // initialize, tools/list.
    let init = c.rpc("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "fsheets");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    c.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let old = c.rpc("initialize", json!({ "protocolVersion": "2024-11-05" }));
    assert_eq!(old["result"]["protocolVersion"], "2024-11-05");
    let list = c.rpc("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in [
        "list_files",
        "open_file",
        "create_file",
        "read_range",
        "write_range",
        "add_sheet",
        "list_sheets",
        "evaluate",
        "export",
        "join_session",
        "leave_session",
        "who_is_here",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    assert!(list["result"]["tools"][0]["inputSchema"]["type"] == "object");
    assert_eq!(c.rpc("nonsense/method", json!({}))["error"]["code"], -32601);
    assert_eq!(
        c.rpc(
            "tools/call",
            json!({ "name": "no_such_tool", "arguments": {} })
        )["error"]["code"],
        -32602
    );

    // list_files finds the shared workbook; open_file shows its sheets.
    let files = c.tool("list_files", json!({})).unwrap();
    assert!(
        files["shared_with_me"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == item.as_str() && f["name"] == "Budget.xlsx")
    );
    let opened = c.tool("open_file", json!({ "item": item })).unwrap();
    assert_eq!(opened["sheets"][0]["name"], "Budget");
    assert_eq!(opened["joined"], false);

    // Read the file's own formulas, calculated by the app's engine.
    let f2 = c.tool("read_range", json!({ "range": "F2" })).unwrap();
    assert_eq!(f2["formulas"][0][0], "=SUM(B2:E2)");
    assert_eq!(f2["values"][0][0], 4450.0);

    // A person is in the workbook; the agent joins and works on B2:C3.
    let mut person = Live::open(&alice, &item, false).unwrap();
    assert!(person.wait_synced(Duration::from_secs(10)));
    let joined = c.tool("join_session", json!({ "range": "B2:C3" })).unwrap();
    assert!(
        joined["people"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["user"] == "alice" && p["agent"] == false)
    );
    person.pump(Duration::from_millis(400));
    let agent = person
        .peers
        .values()
        .find(|p| p.user == "agent")
        .expect("the agent shows in presence");
    assert!(agent.agent);
    let at = agent.presence.as_ref().expect("and says where it works");
    assert_eq!((at.sel.r1, at.sel.c1, at.sel.r2, at.sel.c2), (1, 1, 2, 2));

    // Write values and a formula through the live session: the person sees them.
    let before = versions(&alice, &item);
    let w = c
        .tool(
            "write_range",
            json!({ "range": "H1", "values": [[1, 2], ["=H1+I1", null]] }),
        )
        .unwrap();
    assert_eq!(w["values"], json!([[1.0, 2.0], [3.0, null]]));
    assert!(w["session"].as_str().unwrap().starts_with("joined"));
    let r = c
        .tool("read_range", json!({ "range": "H1:I2", "display": true }))
        .unwrap();
    assert_eq!(r["values"], json!([[1.0, 2.0], [3.0, null]]));
    assert_eq!(r["formulas"], json!([[null, null], ["=H1+I1", null]]));
    assert_eq!(r["display"][1][0], "3");
    person.pump(Duration::from_millis(500));
    let sheet = person.first_sheet().unwrap();
    assert_eq!(
        person.cell_data(sheet, 1, 7).and_then(|d| d.f).as_deref(),
        Some("H1+I1")
    );
    assert_eq!(
        person.workbook().sheets[0].value(1, 7),
        &freedom_sheets::model::Value::Number(3.0)
    );
    // Presence follows the work.
    let agent = person.peers.values().find(|p| p.user == "agent").unwrap();
    let at = agent.presence.as_ref().unwrap();
    assert_eq!((at.sel.r1, at.sel.c1, at.sel.r2, at.sel.c2), (0, 7, 1, 8));

    // Bad input changes nothing and says why.
    let e = c
        .tool(
            "write_range",
            json!({ "range": "K1", "values": [[1, "=SUM("]] }),
        )
        .unwrap_err();
    assert!(e.contains("L1") && e.contains("formula"), "{e}");
    assert!(c.tool("read_range", json!({ "range": "K1" })).unwrap()["values"][0][0].is_null());
    assert!(
        c.tool("write_range", json!({ "range": "A1:B2", "values": [[1]] }))
            .unwrap_err()
            .contains("2x2")
    );
    assert!(
        c.tool("read_range", json!({ "range": "nonsense" }))
            .is_err()
    );

    // Calculate, add a sheet, export.
    let ev = c
        .tool("evaluate", json!({ "formula": "=SUM(H1:I2)*2" }))
        .unwrap();
    assert_eq!(ev["value"], 12.0);
    assert!(c.tool("evaluate", json!({ "formula": "=SUM(" })).is_err());
    let added = c.tool("add_sheet", json!({ "name": "Agent" })).unwrap();
    assert_eq!(added["added"], "Agent");
    assert!(c.tool("add_sheet", json!({ "name": "agent" })).is_err());
    c.tool(
        "write_range",
        json!({ "sheet": "Agent", "range": "A1", "values": [["note", "=Budget!F2"]] }),
    )
    .unwrap();
    let csv = dir.join("out.csv");
    let ex = c
        .tool(
            "export",
            json!({ "path": csv.to_str().unwrap(), "sheet": "Budget" }),
        )
        .unwrap();
    assert_eq!(ex["format"], "csv");
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.contains("Rent,1000,1075,1150,1225,4450"), "{text}");
    assert!(text.lines().next().unwrap().ends_with(",,1,2"), "{text}");
    assert!(
        c.tool("export", json!({ "path": csv.to_str().unwrap() }))
            .unwrap_err()
            .contains("exists")
    );
    let xlsx = dir.join("out.xlsx");
    c.tool("export", json!({ "path": xlsx.to_str().unwrap() }))
        .unwrap();
    let back = freedom_sheets::io::load_xlsx_bytes(&std::fs::read(&xlsx).unwrap()).unwrap();
    assert_eq!(back.sheets.len(), 3);
    assert_eq!(
        back.sheets[0]
            .get(1, 7)
            .unwrap()
            .formula
            .as_ref()
            .unwrap()
            .text,
        "H1+I1"
    );

    // who_is_here from both sides.
    let who = c.tool("who_is_here", json!({})).unwrap();
    assert!(
        who["people"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["user"] == "alice")
    );
    assert!(
        who["people"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["user"] == "agent" && p["you"] == true && p["agent"] == true)
    );
    let people = who["people"].as_array().unwrap();
    let me = people.iter().find(|p| p["you"] == true).unwrap();
    assert_eq!(me["working_on"]["sheet"], "Agent");
    assert_eq!(me["working_on"]["range"], "A1:B1");

    // Leaving saves a version of the file with the agent's work in it.
    let left = c.tool("leave_session", json!({})).unwrap();
    let saved = left["saved_version"]
        .as_i64()
        .expect("a snapshot became a version");
    assert!(versions(&alice, &item) > before);
    assert_eq!(alice.item(&item).unwrap().version, saved);
    person.pump(Duration::from_millis(300));
    assert!(!person.peers.values().any(|p| p.user == "agent"));
    let file = freedom_sheets::io::load_xlsx_bytes(&alice.content(&item).unwrap().body).unwrap();
    assert_eq!(file.sheets.len(), 3);
    assert_eq!(file.sheets[2].name, "Agent");
    assert_eq!(
        file.sheets[0]
            .get(1, 7)
            .unwrap()
            .formula
            .as_ref()
            .unwrap()
            .text,
        "H1+I1"
    );
    drop(person);

    // A person joining now gets exactly that (file + live state).
    let mut fresh = Live::open(&alice, &item, false).unwrap();
    assert!(fresh.wait_synced(Duration::from_secs(10)));
    assert_eq!(
        fresh.workbook().sheets[0].value(1, 7),
        &freedom_sheets::model::Value::Number(3.0)
    );
    assert_eq!(
        fresh.workbook().sheets[2].value(0, 1),
        &freedom_sheets::model::Value::Number(4450.0)
    );
    drop(fresh);

    // Without a joined session a write joins for a moment, saves a version and leaves.
    let before = versions(&alice, &item);
    let one = c
        .tool(
            "write_range",
            json!({ "item": item, "range": "A12", "values": [["one-off"]] }),
        )
        .unwrap();
    assert!(one["session"].as_str().unwrap().starts_with("one-off"));
    assert!(one["saved_version"].as_i64().unwrap() > saved);
    assert_eq!(versions(&alice, &item), before + 1);

    // A new workbook in a folder.
    let own = Api::login(&srv.url, "agent", PW, "test")
        .unwrap()
        .root()
        .unwrap();
    let made = c
        .tool(
            "create_file",
            json!({ "folder": own, "name": "Fresh", "sheets": ["Data", "Notes"] }),
        )
        .unwrap();
    let sheets = c
        .tool("list_sheets", json!({ "item": made["id"] }))
        .unwrap();
    assert_eq!(sheets["sheets"].as_array().unwrap().len(), 2);
    assert_eq!(sheets["sheets"][1]["name"], "Notes");

    // Closing the pipe ends the server cleanly.
    assert!(c.close().success());
}
