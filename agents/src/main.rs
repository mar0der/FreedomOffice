//! `fsheets`: headless client for FreedomOffice documents. Passwords are read from files or
//! standard input, never from arguments; afterwards only a device token (0600 file) is used.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use fsheets::api::Api;
use fsheets::config::{self, Account};
use fsheets::live::Live;
use fsheets::{sample, soak, work};
use serde_json::{Value as Json, json};

const USAGE: &str = "fsheets - headless FreedomOffice client (command line and MCP server for agents)

Accounts: --token-file FILE (a token saved by `login`; default: the only *.token in
~/.config/freedomsheets-agents/). Passwords are only ever read from a file (mode 0600) or
standard input, never from arguments.

USAGE:
    fsheets login --server URL --user NAME [--token-file FILE] [--password-file FILE]
                                  Sign in (password from the file or standard input) and save a token
    fsheets ls [--folder ID]      List the places to start from (My files, organisation folders,
                                  shared with you), or the items of a folder
    fsheets new --folder ID --name NAME [--sheets A,B]
                                  Create an empty workbook in a folder
    fsheets cat --item ID [--sheet NAME] [--range A1:C5] [--formulas]
                                  Print cells as tab separated text (calculated values, or formulas)
    fsheets set --item ID [--sheet NAME] --cell A1 (--value TEXT | --clear)
                                  Edit one cell through the live session (=formulas allowed)
    fsheets write --item ID [--sheet NAME] --range A1 --values JSON
                                  Write a 2D JSON array, e.g. '[[1,2],[\"=A1+B1\",3]]'
    fsheets eval --item ID [--sheet NAME] FORMULA
                                  Calculate a formula against the workbook
    fsheets export --item ID --out FILE [--sheet NAME] [--overwrite]
                                  Save as .xlsx, or one sheet as .csv / .tsv
    fsheets add-sheet --item ID --name NAME
                                  Add a sheet
    fsheets versions --item ID    The version history
    fsheets mcp [--server URL --user NAME --password-file FILE] [--token-file FILE]
                                  MCP server on standard input/output for AI agents
    fsheets soak --server URL --item ID --users a,b,c --secrets-dir DIR
                 [--clients 4] [--edits 150] [--seed N] [--interval-ms 40]
                                  Several clients edit at random, disconnect and reconnect; all must converge
    fsheets probe --server URL --item ID --users a,b --secrets-dir DIR
                                  Show what concurrent edits of one cell do
    fsheets seed --token-file FILE --folder ID
                                  Upload the sample workbooks into a folder
    fsheets make-sample KIND FILE Write a sample .xlsx (budget, tasks, grades)
";

fn opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 >= args.len() {
        args.remove(i);
        return None;
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

fn flag(args: &mut Vec<String>, name: &str) -> bool {
    args.iter()
        .position(|a| a == name)
        .map(|i| args.remove(i))
        .is_some()
}

fn need(v: Option<String>, name: &str) -> Result<String, String> {
    v.ok_or_else(|| format!("Missing {name}. See --help."))
}

fn read_password(file: Option<String>) -> Result<String, String> {
    let mut s = String::new();
    match file {
        Some(f) => {
            s = std::fs::read_to_string(&f).map_err(|e| format!("Couldn't read {f}: {e}"))?
        }
        None => {
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

fn logins(server: &str, users: &str, dir: &str) -> Result<Vec<Api>, String> {
    users
        .split(',')
        .filter(|u| !u.is_empty())
        .map(|u| {
            let pw = read_password(Some(
                Path::new(dir)
                    .join(format!("{u}.pw"))
                    .to_string_lossy()
                    .into_owned(),
            ))?;
            Api::login(server, u, &pw, "fsheets")
        })
        .collect()
}

/// Joins the item's live session (so the current state, including what was edited live since
/// the last saved version, is replayed) and waits until caught up.
fn join(api: &Api, item: &str) -> Result<Live, String> {
    let mut live = Live::open(api, item, true)?;
    if !live.wait_synced(Duration::from_secs(20)) {
        return Err(live
            .stopped
            .clone()
            .unwrap_or_else(|| "Couldn't join the live session.".into()));
    }
    Ok(live)
}

/// A change through the live session; the version it was saved as is reported.
fn change<T>(
    api: &Api,
    item: &str,
    f: impl FnOnce(&mut Live) -> Result<T, String>,
) -> Result<(T, Option<i64>), String> {
    let mut live = join(api, item)?;
    let out = f(&mut live)?;
    let version = work::settle(&mut live)?;
    live.disconnect();
    Ok((out, version))
}

fn print_json(v: &Json) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn run(mut args: Vec<String>) -> Result<(), String> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    let cmd = args.remove(0);
    let token = |a: &mut Vec<String>| -> Result<Api, String> {
        config::connect(&Account {
            token_file: opt(a, "--token-file").map(PathBuf::from),
            ..Default::default()
        })
    };
    match cmd.as_str() {
        "login" => {
            let server = need(opt(&mut args, "--server"), "--server")?;
            let user = need(opt(&mut args, "--user"), "--user")?;
            let file = opt(&mut args, "--token-file")
                .map(PathBuf::from)
                .or_else(|| config::token_path(&server, &user))
                .ok_or("Missing --token-file (no home directory to keep it in).")?;
            let pw = read_password(opt(&mut args, "--password-file"))?;
            let api = Api::login(&server, &user, &pw, "fsheets")?;
            config::save_token(&api, &file)?;
            println!(
                "Signed in as {} on {}; token saved to {}.",
                api.username,
                api.url,
                file.display()
            );
        }
        "ls" => {
            let api = token(&mut args)?;
            let folder = opt(&mut args, "--folder");
            let items = match folder {
                Some(f) => api.children(&f)?,
                None => {
                    for (name, root) in api.org_roots()? {
                        println!("org {name}\tfolder {root}");
                    }
                    println!("my files\tfolder {}", api.root()?);
                    api.shared_with_me()?
                }
            };
            for i in items {
                println!(
                    "{}\t{}\t{}\tv{}\t{}",
                    i.id,
                    if i.folder { "folder" } else { "file" },
                    i.name,
                    i.version,
                    i.role
                );
            }
        }
        "new" => {
            let api = token(&mut args)?;
            let folder = need(opt(&mut args, "--folder"), "--folder")?;
            let name = need(opt(&mut args, "--name"), "--name")?;
            let sheets: Vec<String> = opt(&mut args, "--sheets")
                .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                .unwrap_or_default();
            let it = work::create_file(&api, &folder, &name, &sheets)?;
            println!("{}\t{}", it.id, it.name);
        }
        "cat" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let sheet = opt(&mut args, "--sheet");
            let range = opt(&mut args, "--range");
            let formulas = flag(&mut args, "--formulas");
            let mut live = join(&api, &item)?;
            let names: Vec<String> = match (&sheet, &range) {
                (None, None) => live.wb.sheets.iter().map(|s| s.name.clone()).collect(),
                (Some(s), _) => vec![s.clone()],
                (None, Some(_)) => vec![String::new()],
            };
            let multi = names.len() > 1;
            for name in names {
                let sheet = (!name.is_empty()).then_some(name.as_str());
                let r = work::read_range(&mut live, sheet, range.as_deref(), true);
                let r = match r {
                    Ok(r) => r,
                    // An empty sheet has nothing to print.
                    Err(e) if range.is_none() && e == "The sheet is empty." => continue,
                    Err(e) => return Err(e),
                };
                if multi {
                    println!("# {} ({})", r["sheet"].as_str().unwrap_or(""), r["range"].as_str().unwrap_or(""));
                }
                let cells = |k: &str| r[k].as_array().cloned().unwrap_or_default();
                for (row, f) in cells(if formulas { "formulas" } else { "display" }).iter().zip(cells("formulas")) {
                    let line: Vec<String> = row
                        .as_array()
                        .into_iter()
                        .flatten()
                        .zip(f.as_array().into_iter().flatten())
                        .map(|(shown, formula)| {
                            if formulas && !formula.is_null() {
                                formula.as_str().unwrap_or("").to_string()
                            } else {
                                shown.as_str().map(str::to_string).unwrap_or_else(|| if shown.is_null() { String::new() } else { shown.to_string() })
                            }
                        })
                        .collect();
                    println!("{}", line.join("\t"));
                }
            }
        }
        "set" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let cell = need(opt(&mut args, "--cell"), "--cell")?;
            let clear = flag(&mut args, "--clear");
            let value = opt(&mut args, "--value");
            let sheet = opt(&mut args, "--sheet");
            let v = if clear { Json::Null } else { json!(need(value, "--value or --clear")?) };
            let (out, saved) = change(&api, &item, |l| work::write_range(l, sheet.as_deref(), &cell, &[vec![v]]))?;
            println!(
                "Set {} to {} (confirmed{}).",
                cell,
                out["values"][0][0],
                saved.map(|v| format!(", saved as version {v}")).unwrap_or_default()
            );
        }
        "write" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let range = need(opt(&mut args, "--range"), "--range")?;
            let sheet = opt(&mut args, "--sheet");
            let values = need(opt(&mut args, "--values"), "--values")?;
            let grid: Json = serde_json::from_str(&values).map_err(|e| format!("--values isn't JSON: {e}"))?;
            let grid = match grid {
                Json::Array(rows) if rows.iter().all(Json::is_array) => rows
                    .iter()
                    .map(|r| r.as_array().cloned().unwrap_or_default())
                    .collect::<Vec<_>>(),
                _ => return Err("--values must be a 2D JSON array like [[1,2],[3,4]].".into()),
            };
            let (out, saved) = change(&api, &item, |l| work::write_range(l, sheet.as_deref(), &range, &grid))?;
            print_json(&out);
            if let Some(v) = saved {
                eprintln!("saved as version {v}");
            }
        }
        "eval" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let sheet = opt(&mut args, "--sheet");
            let [formula] = args.as_slice() else {
                return Err("Expected one formula, e.g. fsheets eval --item ID \"=SUM(A1:A5)\".".into());
            };
            let mut live = join(&api, &item)?;
            let out = work::evaluate(&mut live, sheet.as_deref(), formula)?;
            println!("{}", out["display"].as_str().unwrap_or(""));
        }
        "export" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let out = PathBuf::from(need(opt(&mut args, "--out"), "--out")?);
            let sheet = opt(&mut args, "--sheet");
            let overwrite = flag(&mut args, "--overwrite");
            let mut live = join(&api, &item)?;
            let r = work::export(&mut live, &out, sheet.as_deref(), overwrite)?;
            println!("Wrote {} ({} bytes).", out.display(), r["bytes"]);
        }
        "add-sheet" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let name = need(opt(&mut args, "--name"), "--name")?;
            let (out, saved) = change(&api, &item, |l| work::add_sheet(l, &name))?;
            println!(
                "Added sheet {}{}.",
                out["added"].as_str().unwrap_or(""),
                saved.map(|v| format!(" (saved as version {v})")).unwrap_or_default()
            );
        }
        "versions" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            for v in work::versions(&api, &item)?["versions"].as_array().into_iter().flatten() {
                println!(
                    "v{}\t{}\t{}\t{} bytes{}",
                    v["version"],
                    v["modified"].as_str().unwrap_or(""),
                    v["modified_by"].as_str().unwrap_or(""),
                    v["size"],
                    if v["current"].as_bool().unwrap_or(false) { "\tcurrent" } else { "" }
                );
            }
        }
        "mcp" => {
            let acct = Account {
                server: opt(&mut args, "--server"),
                user: opt(&mut args, "--user"),
                password_file: opt(&mut args, "--password-file").map(PathBuf::from),
                token_file: opt(&mut args, "--token-file").map(PathBuf::from),
            };
            let api = config::connect(&acct)?;
            fsheets::mcp::serve(api, std::io::BufReader::new(std::io::stdin()), std::io::stdout())?;
        }
        "soak" => {
            let server = need(opt(&mut args, "--server"), "--server")?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let apis = logins(
                &server,
                &need(opt(&mut args, "--users"), "--users")?,
                &need(opt(&mut args, "--secrets-dir"), "--secrets-dir")?,
            )?;
            let mut cfg = soak::Config::default();
            if let Some(v) = opt(&mut args, "--clients") {
                cfg.clients = v.parse().map_err(|_| "--clients must be a number")?;
            }
            if let Some(v) = opt(&mut args, "--edits") {
                cfg.edits = v.parse().map_err(|_| "--edits must be a number")?;
            }
            if let Some(v) = opt(&mut args, "--interval-ms") {
                cfg.interval_ms = v.parse().map_err(|_| "--interval-ms must be a number")?;
            }
            cfg.seed = match opt(&mut args, "--seed") {
                Some(v) => v.parse().map_err(|_| "--seed must be a number")?,
                None => fsheets::rng::Rng::random_seed(),
            };
            let report = soak::run(&apis, &item, &cfg)?;
            report.print();
            if !report.converged() {
                return Err("The clients did not converge.".into());
            }
        }
        "probe" => {
            let server = need(opt(&mut args, "--server"), "--server")?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let apis = logins(
                &server,
                &need(opt(&mut args, "--users"), "--users")?,
                &need(opt(&mut args, "--secrets-dir"), "--secrets-dir")?,
            )?;
            let b = apis.get(1).unwrap_or(&apis[0]);
            let p = soak::probe_conflicts(&apis[0], b, &item)?;
            println!(
                "{}\n  value kept: {}, format kept: {}\n{}\n{}",
                p.value_vs_format,
                p.value_survived,
                p.format_survived,
                p.value_vs_value,
                p.insert_vs_edit
            );
        }
        "seed" => {
            let api = token(&mut args)?;
            let folder = need(opt(&mut args, "--folder"), "--folder")?;
            let existing: Vec<String> =
                api.children(&folder)?.into_iter().map(|i| i.name).collect();
            for kind in sample::SAMPLES {
                let name = format!("{}.xlsx", kind[..1].to_uppercase() + &kind[1..]);
                if existing.contains(&name) {
                    println!("{name} exists; skipped.");
                    continue;
                }
                let it = api.upload_new(&folder, &name, sample::make(kind)?)?;
                println!("{}\t{}", it.id, it.name);
            }
        }
        "make-sample" => {
            let [kind, file] = args.as_slice() else {
                return Err("Expected `make-sample KIND FILE`.".into());
            };
            std::fs::write(file, sample::make(kind)?).map_err(|e| e.to_string())?;
        }
        other => return Err(format!("Unknown command '{other}'. See --help.")),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
