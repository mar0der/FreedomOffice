//! `freedomoffice-server`: the server and its admin commands.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use freedomoffice_server::config::{self, Config};
use freedomoffice_server::store::{OrgRole, Store, User};
use freedomoffice_server::util::iso;
use freedomoffice_server::{open_store, serve};

const USAGE: &str = "freedomoffice-server - FreedomOffice document server

USAGE:
    freedomoffice-server [--config FILE] <COMMAND>

COMMANDS:
    init [--listen ADDR] [--data-dir DIR] [--public-url URL]
         [--tls-domain DOMAIN] [--acme-email EMAIL] [--admin USERNAME]
                                 Create the configuration (if missing), the data directory
                                 and the first administrator (password read from the terminal
                                 or from standard input)
    serve                        Run the server
    user add USERNAME [--display-name NAME] [--email EMAIL] [--admin]
    user list
    user disable USERNAME        Block sign-in and revoke the user's devices
    user enable USERNAME
    user reset-password USERNAME
    user make-admin USERNAME
    org add NAME                 Create an organisation (with its own folder)
    org list
    org members ORG
    org add-member ORG USERNAME [member|admin|owner]
                                 Add a person (default: member) or change their role
    org remove-member ORG USERNAME
    tokens list USERNAME         Signed-in devices of a user
    tokens revoke USERNAME       Sign a user out of every device
    backup DIR                   Consistent copy of the database and all documents into DIR
                                 (files already in DIR are skipped; run nightly, then rsync DIR)
    audit-prune --before YYYY-MM-DD [--user USERNAME]
                                 Erase audit events older than the date (UTC), optionally only
                                 those done by or to one person (e.g. a request to delete their
                                 data); the erasure itself is recorded. Stop the server first.

OPTIONS:
    --config FILE   Configuration file (default /etc/freedomoffice/server.toml)
    -h, --help      Show this help
    -V, --version   Show the version
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") || args.is_empty() {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("freedomoffice-server {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let config_path = take_opt(&mut args, "--config").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(config::DEFAULT_CONFIG));
    match run(&config_path, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Removes `--name VALUE` from the arguments.
fn take_opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 >= args.len() {
        args.remove(i);
        return None;
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

fn load(config_path: &Path) -> Result<Config, String> {
    Config::load(config_path).map_err(|e| format!("{e}\n(Run `freedomoffice-server init` first, or pass --config FILE.)"))
}

fn store(config_path: &Path) -> Result<Store, String> {
    open_store(&load(config_path)?)
}

fn user(s: &Store, name: &str) -> Result<User, String> {
    s.user_by_name(name).map_err(|e| e.to_string())?.ok_or_else(|| format!("There's no user named '{name}'."))
}

fn one_arg(args: &[String], what: &str) -> Result<String, String> {
    match args {
        [a] if !a.starts_with('-') => Ok(a.clone()),
        _ => Err(format!("Expected {what}. See --help.")),
    }
}

fn run(config_path: &Path, mut args: Vec<String>) -> Result<(), String> {
    let cmd = args.remove(0);
    match cmd.as_str() {
        "serve" => {
            let cfg = load(config_path)?;
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr)
                .init();
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| e.to_string())?;
            rt.block_on(serve(cfg))
        }
        "init" => init(config_path, args),
        "user" => {
            if args.is_empty() {
                return Err("Expected a user command. See --help.".into());
            }
            let sub = args.remove(0);
            let s = store(config_path)?;
            match sub.as_str() {
                "add" => {
                    let display = take_opt(&mut args, "--display-name").unwrap_or_default();
                    let email = take_opt(&mut args, "--email").unwrap_or_default();
                    let admin = take_flag(&mut args, "--admin");
                    let name = one_arg(&args, "a username")?;
                    let pw = read_new_password(&format!("Password for {name}"), &name)?;
                    let u = s.create_user(&name, &display, &email, &pw, admin).map_err(|e| e.to_string())?;
                    println!("Created {}{}.", u.username, if u.is_admin { " (administrator)" } else { "" });
                }
                "list" => {
                    println!("{:<24} {:<28} {:<6} {:<9} CREATED", "USERNAME", "DISPLAY NAME", "ADMIN", "STATUS");
                    for u in s.users().map_err(|e| e.to_string())? {
                        println!(
                            "{:<24} {:<28} {:<6} {:<9} {}",
                            u.username,
                            u.display_name,
                            if u.is_admin { "yes" } else { "" },
                            if u.disabled { "disabled" } else { "active" },
                            iso(u.created)
                        );
                    }
                }
                "disable" | "enable" => {
                    let u = user(&s, &one_arg(&args, "a username")?)?;
                    s.set_disabled(&u.id, sub == "disable").map_err(|e| e.to_string())?;
                    println!("{} is now {}.", u.username, if sub == "disable" { "disabled (all devices signed out)" } else { "enabled" });
                }
                "reset-password" => {
                    let u = user(&s, &one_arg(&args, "a username")?)?;
                    let pw = read_new_password(&format!("New password for {}", u.username), &u.username)?;
                    s.set_password(&u.id, &pw).map_err(|e| e.to_string())?;
                    let n = s.revoke_user_tokens(&u.id).map_err(|e| e.to_string())?;
                    println!("Password changed; {n} device(s) signed out.");
                }
                "make-admin" => {
                    let u = user(&s, &one_arg(&args, "a username")?)?;
                    s.set_admin(&u.id, true).map_err(|e| e.to_string())?;
                    println!("{} is now an administrator.", u.username);
                }
                other => return Err(format!("Unknown user command '{other}'. See --help.")),
            }
            Ok(())
        }
        "org" => {
            if args.is_empty() {
                return Err("Expected an org command. See --help.".into());
            }
            let sub = args.remove(0);
            let s = store(config_path)?;
            let org = |name: &str| s.find_org(name).map_err(|e| e.to_string())?.ok_or_else(|| format!("There's no organisation named '{name}'."));
            match sub.as_str() {
                "add" => {
                    let name = one_arg(&args, "an organisation name")?;
                    let o = s.create_org(&name, "cli").map_err(|e| e.to_string())?;
                    println!("Created the organisation {} (folder {}).", o.name, o.root_id);
                }
                "list" => {
                    println!("{:<32} {:<8} {:<8} CREATED", "NAME", "MEMBERS", "DEFAULT");
                    for o in s.orgs().map_err(|e| e.to_string())? {
                        let n = s.members(&o.id).map_err(|e| e.to_string())?.len();
                        println!("{:<32} {:<8} {:<8} {}", o.name, n, o.member_role.as_str(), iso(o.created));
                    }
                }
                "members" => {
                    let o = org(&one_arg(&args, "an organisation name")?)?;
                    for m in s.members(&o.id).map_err(|e| e.to_string())? {
                        println!("{:<24} {:<28} {}", m.user.username, m.user.display_name, m.role.as_str());
                    }
                }
                "add-member" => {
                    let (o, u, role) = match args.as_slice() {
                        [o, u] => (o.clone(), u.clone(), "member".to_string()),
                        [o, u, r] => (o.clone(), u.clone(), r.clone()),
                        _ => return Err("Expected `org add-member ORG USERNAME [member|admin|owner]`.".into()),
                    };
                    let role = OrgRole::parse(&role).ok_or("The role must be member, admin or owner.")?;
                    let (o, u) = (org(&o)?, user(&s, &u)?);
                    let added = s.set_member(&o, &u, role, "cli").map_err(|e| e.to_string())?;
                    println!("{} {} {} of {}.", u.username, if added { "joined as" } else { "is now" }, role.as_str(), o.name);
                }
                "remove-member" => {
                    let [o, u] = args.as_slice() else { return Err("Expected `org remove-member ORG USERNAME`.".into()) };
                    let (o, u) = (org(o)?, user(&s, u)?);
                    if !s.remove_member(&o.id, &u.id).map_err(|e| e.to_string())? {
                        return Err(format!("{} isn't a member of {}.", u.username, o.name));
                    }
                    println!("Removed {} from {}.", u.username, o.name);
                }
                other => return Err(format!("Unknown org command '{other}'. See --help.")),
            }
            Ok(())
        }
        "tokens" => {
            if args.is_empty() {
                return Err("Expected `tokens list USERNAME` or `tokens revoke USERNAME`.".into());
            }
            let sub = args.remove(0);
            let s = store(config_path)?;
            let u = user(&s, &one_arg(&args, "a username")?)?;
            match sub.as_str() {
                "list" => {
                    for t in s.tokens_of(&u.id).map_err(|e| e.to_string())? {
                        println!("{:<40} signed in {}  last used {}", t.device, iso(t.created), iso(t.last_used));
                    }
                }
                "revoke" => {
                    let n = s.revoke_user_tokens(&u.id).map_err(|e| e.to_string())?;
                    println!("Signed {} out of {n} device(s).", u.username);
                }
                other => return Err(format!("Unknown tokens command '{other}'.")),
            }
            Ok(())
        }
        "backup" => {
            let dir = PathBuf::from(one_arg(&args, "a backup directory")?);
            let s = store(config_path)?;
            let (n, bytes) = s.backup(&dir).map_err(|e| e.to_string())?;
            println!("Backed up the database and {n} new document version(s) ({:.1} MB) to {}.", bytes as f64 / 1048576.0, dir.display());
            Ok(())
        }
        "audit-prune" => {
            let before = take_opt(&mut args, "--before").ok_or("Expected --before YYYY-MM-DD. See --help.")?;
            let user = take_opt(&mut args, "--user");
            if !args.is_empty() {
                return Err("Expected `audit-prune --before YYYY-MM-DD [--user USERNAME]`.".into());
            }
            let at = freedomoffice_server::util::parse_date(&before).ok_or("--before must be a date like 2026-01-31.")?;
            let s = store(config_path)?;
            let n = s.audit_erase(at, user.as_deref(), "cli").map_err(|e| e.to_string())?;
            println!("Erased {n} audit event(s) before {before}{}.", if user.is_some() { " done by or to that person" } else { "" });
            Ok(())
        }
        other => Err(format!("Unknown command '{other}'. See --help.")),
    }
}

fn init(config_path: &Path, mut args: Vec<String>) -> Result<(), String> {
    let listen = take_opt(&mut args, "--listen");
    let data_dir = take_opt(&mut args, "--data-dir");
    let public_url = take_opt(&mut args, "--public-url");
    let tls_domain = take_opt(&mut args, "--tls-domain");
    let acme_email = take_opt(&mut args, "--acme-email");
    let admin = take_opt(&mut args, "--admin");
    if let Some(a) = args.first() {
        return Err(format!("Unexpected argument '{a}'. See --help."));
    }
    if config_path.exists() {
        println!("Using the existing configuration {}.", config_path.display());
    } else {
        let listen = listen.unwrap_or_else(|| if tls_domain.is_some() { "0.0.0.0:443".into() } else { "127.0.0.1:8080".into() });
        let data_dir = PathBuf::from(data_dir.unwrap_or_else(|| config::DEFAULT_DATA_DIR.into()));
        let public_url = public_url.or_else(|| tls_domain.as_ref().map(|d| format!("https://{d}"))).unwrap_or_default();
        let text = config::template(&listen, &data_dir, &public_url, tls_domain.as_deref(), acme_email.as_deref());
        Config::parse(&text)?;
        if let Some(d) = config_path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d).map_err(|e| format!("Couldn't create {}: {e}", d.display()))?;
        }
        std::fs::write(config_path, text).map_err(|e| format!("Couldn't write {}: {e}", config_path.display()))?;
        println!("Wrote {}.", config_path.display());
    }
    let cfg = load(config_path)?;
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| format!("Couldn't create {}: {e}", cfg.data_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&cfg.data_dir, std::fs::Permissions::from_mode(0o750));
    }
    let s = open_store(&cfg)?;
    if !s.users().map_err(|e| e.to_string())?.is_empty() {
        println!("The data directory {} already has users; no administrator created.", cfg.data_dir.display());
        return Ok(());
    }
    let name = match admin {
        Some(a) => a,
        None => {
            let a = prompt_line("Administrator username [admin]: ")?;
            if a.trim().is_empty() { "admin".into() } else { a.trim().to_string() }
        }
    };
    let pw = read_new_password(&format!("Password for {name}"), &name)?;
    let u = s.create_user(&name, "", "", &pw, true).map_err(|e| e.to_string())?;
    println!("Created the administrator {}. Start the server with `freedomoffice-server serve`.", u.username);
    Ok(())
}

fn prompt_line(prompt: &str) -> Result<String, String> {
    if std::io::stdin().is_terminal() {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// From the terminal without echo (asked twice), or one line of standard input when piped.
/// Never from the command line, where it would end up in shell history and `ps`.
fn read_new_password(what: &str, username: &str) -> Result<String, String> {
    if std::io::stdin().is_terminal() {
        let a = rpassword::prompt_password(format!("{what}: ")).map_err(|e| e.to_string())?;
        let b = rpassword::prompt_password("Repeat the password: ").map_err(|e| e.to_string())?;
        if a != b {
            return Err("The passwords don't match.".into());
        }
        freedomoffice_server::util::check_new_password(&a, username)?;
        Ok(a)
    } else {
        let pw = prompt_line("")?;
        freedomoffice_server::util::check_new_password(&pw, username)?;
        Ok(pw)
    }
}
