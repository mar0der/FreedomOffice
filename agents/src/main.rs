//! `fsheets`: headless client for FreedomOffice documents. Passwords are read from files or
//! standard input, never from arguments; afterwards only a device token (0600 file) is used.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use cosync::{CellData, Val};
use fsheets::api::Api;
use fsheets::book::{a1, parse_a1, val_text};
use fsheets::live::Live;
use fsheets::{sample, soak};

const USAGE: &str = "fsheets - headless FreedomOffice client

USAGE:
    fsheets login --server URL --user NAME --token-file FILE [--password-file FILE]
                                  Sign in (password from the file or standard input) and save a token
    fsheets ls --token-file FILE [--folder ID]
                                  List organisation folders, or the items of a folder
    fsheets cat --token-file FILE --item ID [--sheet NAME]
                                  Open the live session and print the sheet as tab separated text
    fsheets set --token-file FILE --item ID [--sheet NAME] --cell A1 (--value TEXT | --clear)
                                  Edit one cell through the live session
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
    args.iter().position(|a| a == name).map(|i| args.remove(i)).is_some()
}

fn need(v: Option<String>, name: &str) -> Result<String, String> {
    v.ok_or_else(|| format!("Missing {name}. See --help."))
}

fn read_password(file: Option<String>) -> Result<String, String> {
    let mut s = String::new();
    match file {
        Some(f) => s = std::fs::read_to_string(&f).map_err(|e| format!("Couldn't read {f}: {e}"))?,
        None => {
            std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
        }
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

fn logins(server: &str, users: &str, dir: &str) -> Result<Vec<Api>, String> {
    users
        .split(',')
        .filter(|u| !u.is_empty())
        .map(|u| {
            let pw = read_password(Some(Path::new(dir).join(format!("{u}.pw")).to_string_lossy().into_owned()))?;
            Api::login(server, u, &pw, "fsheets")
        })
        .collect()
}

fn run(mut args: Vec<String>) -> Result<(), String> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    let cmd = args.remove(0);
    let token = |a: &mut Vec<String>| -> Result<Api, String> { Api::from_token_file(&PathBuf::from(need(opt(a, "--token-file"), "--token-file")?)) };
    match cmd.as_str() {
        "login" => {
            let server = need(opt(&mut args, "--server"), "--server")?;
            let user = need(opt(&mut args, "--user"), "--user")?;
            let file = PathBuf::from(need(opt(&mut args, "--token-file"), "--token-file")?);
            let pw = read_password(opt(&mut args, "--password-file"))?;
            let api = Api::login(&server, &user, &pw, "fsheets")?;
            api.save_token(&file)?;
            println!("Signed in as {} on {}; token saved to {}.", api.username, api.url, file.display());
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
                    api.children(&api.root()?)?
                }
            };
            for i in items {
                println!("{}\t{}\t{}\tv{}\t{}", i.id, if i.folder { "folder" } else { "file" }, i.name, i.version, i.role);
            }
        }
        "cat" => {
            let api = token(&mut args)?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let sheet = opt(&mut args, "--sheet");
            let mut live = Live::open(&api, &item, true)?;
            live.wait_synced(Duration::from_secs(15));
            for n in &live.notes {
                eprintln!("note: {n}");
            }
            for (id, sh) in live.book.ordered() {
                if sheet.as_ref().is_some_and(|s| live.book.find(s) != Some(id)) {
                    continue;
                }
                println!("# {} (sheet {id})", sh.name);
                let (rows, cols) = sh.cells.keys().fold((0, 0), |(r, c), k| (r.max(k.0 + 1), c.max(k.1 + 1)));
                for r in 0..rows {
                    let line: Vec<String> = (0..cols)
                        .map(|c| match sh.cells.get(&(r, c)) {
                            Some(d) => match &d.f {
                                Some(f) => format!("={f}"),
                                None => val_text(&d.v),
                            },
                            None => String::new(),
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
            let (r, c) = parse_a1(&cell).ok_or("--cell must look like B7.")?;
            let clear = flag(&mut args, "--clear");
            let value = opt(&mut args, "--value");
            let sheet = opt(&mut args, "--sheet");
            let mut live = Live::open(&api, &item, true)?;
            live.wait_synced(Duration::from_secs(15));
            let s = match sheet {
                Some(n) => live.book.find(&n).ok_or("No such sheet.")?,
                None => live.book.ordered().first().map(|x| x.0).ok_or("No sheet.")?,
            };
            let data = if clear {
                None
            } else {
                let v = need(value, "--value or --clear")?;
                Some(match v.strip_prefix('=') {
                    Some(f) => CellData { f: Some(f.to_string()), ..Default::default() },
                    None => CellData { v: v.parse::<f64>().map(Val::Num).unwrap_or(Val::Text(v)), ..Default::default() },
                })
            };
            live.set_cell(s, r, c, data);
            if !live.wait_synced(Duration::from_secs(15)) {
                return Err("The server didn't confirm the change.".into());
            }
            println!("Set {} (confirmed at seq {}).", a1(r, c), live.seq());
        }
        "soak" => {
            let server = need(opt(&mut args, "--server"), "--server")?;
            let item = need(opt(&mut args, "--item"), "--item")?;
            let apis = logins(&server, &need(opt(&mut args, "--users"), "--users")?, &need(opt(&mut args, "--secrets-dir"), "--secrets-dir")?)?;
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
            let apis = logins(&server, &need(opt(&mut args, "--users"), "--users")?, &need(opt(&mut args, "--secrets-dir"), "--secrets-dir")?)?;
            let b = apis.get(1).unwrap_or(&apis[0]);
            let p = soak::probe_conflicts(&apis[0], b, &item)?;
            println!("{}\n  value kept: {}, format kept: {}\n{}\n{}", p.value_vs_format, p.value_survived, p.format_survived, p.value_vs_value, p.insert_vs_edit);
        }
        "seed" => {
            let api = token(&mut args)?;
            let folder = need(opt(&mut args, "--folder"), "--folder")?;
            let existing: Vec<String> = api.children(&folder)?.into_iter().map(|i| i.name).collect();
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
            let [kind, file] = args.as_slice() else { return Err("Expected `make-sample KIND FILE`.".into()) };
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
