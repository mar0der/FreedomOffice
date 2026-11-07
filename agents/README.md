# fsheets: headless FreedomOffice client (agents and scripted tests)

Package `freedomsheets-agents`, library `fsheets`, binary `fsheets`. One client for tests and for
agents (DECISIONS #9): sign in, list, open an item's live session over the WebSocket protocol of
`cosync`, keep the workbook in memory (the app's own model and formula engine, through the
`freedom_sheets` library), send edits and presence, answer the server's snapshot requests.
Every Freedom app is meant to be natively usable by AI agents through both a CLI and an MCP
server; for Sheets that is this crate.

| Module | What |
|---|---|
| `api` | HTTP: login (password only in that one request), `/api/me`, root/children, shared-with-me, content, upload, versions, live state and snapshot, token file (0600) |
| `config` | sign-in without passwords on the command line: password file (0600) or cached token (0600) under `~/.config/freedomsheets-agents/` |
| `live` | one live session: `Live::open` (downloads the file and its lossless live state, joins like the app), `pump`, `edit`/`set_cell`, `send_presence`/`send_selection`, `disconnect`/`connect` (resumes from the last seq, resends the unacknowledged batch), `synced()`, `snapshot()` (answers the server's `Snapshot` request: .xlsx plus live state to `/live/snapshot`, which becomes a version). OT bookkeeping is `cosync::client::Session`, the code the app runs; remote operations are applied with `freedom_sheets::live::apply_ops` to a real `Workbook` (formats, merges, notes and all) |
| `a1` | A1 notation: `B7`, `A1:C5`, `A:C`, `3:5`, `Data!A1:B2`, `'My sheet'!A1` |
| `work` | what an agent does with an open workbook: read/write ranges, calculate, add sheets, export, peers, versions. The CLI and MCP are two front ends of it |
| `mcp` | the MCP server (JSON-RPC 2.0 over stdio, hand written: `rmcp` is not in the offline cargo cache) |
| `soak` | the multi-client harness and `probe_conflicts` |
| `sample` | three sample workbooks (rust_xlsxwriter) |

Agents are marked in presence: `agent=1` on the live URL makes the server set `Peer.agent`
(`#[serde(default, skip_serializing_if = "Not::not")]`, so the wire format is unchanged for
people and older apps). Tested in `server/tests/live.rs` and `cosync/src/tests.rs`.

## Commands

```
fsheets login --server URL --user NAME [--token-file F] [--password-file F]   # password from file or stdin
fsheets ls    [--folder ID]                      # My files, org folders, shared with you; or a folder
fsheets new   --folder ID --name NAME [--sheets A,B]
fsheets cat   --item ID [--sheet NAME] [--range A1:C5] [--formulas]   # calculated values by default
fsheets set   --item ID [--sheet NAME] --cell B7 (--value TEXT | --clear)   # =formulas allowed
fsheets write --item ID [--sheet NAME] --range A1 --values '[[1,2],["=A1+B1",3]]'
fsheets eval  --item ID [--sheet NAME] "=SUM(A1:A5)"
fsheets export --item ID --out FILE.xlsx|csv|tsv [--sheet NAME] [--overwrite]
fsheets add-sheet --item ID --name NAME
fsheets versions --item ID
fsheets mcp   [--server URL --user NAME --password-file F] [--token-file F]
fsheets soak  --server URL --item ID --users a,b,c --secrets-dir DIR [--clients 4] [--edits 150] [--seed N] [--restart-pct 2] [--journal-dir D]
fsheets logout --token-file F    # end the sign-in on the server and delete the file
fsheets journal [--discard ID]   # changes an earlier run made but the server never confirmed
fsheets probe --server URL --item ID --users a,b --secrets-dir DIR
fsheets seed  --token-file F --folder ID
```

`soak`/`probe` read `DIR/<user>.pw`. Nothing takes a password as an argument. Without
`--token-file`, the commands use the only `*.token` in `~/.config/freedomsheets-agents/`.
Changing commands (`set`, `write`, `add-sheet`) join the live session, wait for the server's
acknowledgement, save a snapshot (a version in the history, as the app does when it leaves) and
leave. Every `fsheets` session journals what the server hasn't acknowledged (`~/.local/share/freedomsheets-agents/journal`,
mode 0600; `$FSHEETS_JOURNAL_DIR`): if the process dies or the connection never comes back, the
next command or MCP call on that workbook sends those changes first and says so
(`resumed_from_journal`). A command that cannot get the acknowledgement fails with an error saying so;
it never reports success early. `soak` kills clients right after an edit and restarts them from their
journals (`--restart-pct`, default 2); it fails if any journal is left.

## Use with Claude / other agents

`fsheets mcp` is a Model Context Protocol server on standard input/output (newline delimited
JSON-RPC 2.0, protocol revision 2025-06-18; 2025-03-26 and 2024-11-05 are accepted). It has no
network listener: the agent's host starts the process and talks to it through pipes; stdout
carries protocol messages only.

Sign in with the account of the agent (create one such as `agent-claude` on the server). The
password is read once from a file that only you can read (`chmod 600`, first line), the device
token is cached next to it (`~/.config/freedomsheets-agents/<user>@<host>.token`, 0600) and used
from then on; a revoked token falls back to the password file. Passwords are never arguments and
are never logged. (`fsheets login --server URL --user NAME` can do the first sign in by hand,
reading the password from a file or standard input.)

Claude Code (`.mcp.json` in a project, or `claude mcp add`):

```json
{
  "mcpServers": {
    "freedomsheets": {
      "command": "/path/to/fsheets",
      "args": ["mcp", "--server", "https://your-server.example:8090", "--user", "agent-claude",
               "--password-file", "/path/to/agent-claude.pw"]
    }
  }
}
```

Claude Desktop (`claude_desktop_config.json`), same shape:

```json
{
  "mcpServers": {
    "freedomsheets": {
      "command": "/path/to/fsheets",
      "args": ["mcp", "--server", "https://your-server.example:8090", "--user", "agent-claude",
               "--password-file", "/path/to/agent-claude.pw"]
    }
  }
}
```

Once the token is cached, `"args": ["mcp"]` is enough (or `--token-file /path/to/x.token`).

| Tool | What |
|---|---|
| `list_files` | no `folder`: My files (folder id), organisation folders, files shared with you; with `folder`: its files |
| `open_file`, `create_file` | open a workbook (sheets and used ranges; becomes the default `item`); create an empty .xlsx in a folder |
| `list_sheets`, `add_sheet` | sheets with used ranges; add a sheet |
| `read_range` | A1 range (`B7`, `A1:C5`, `A:C`, `Data!A1:B2`, none = used range) as calculated `values` and `formulas` 2D arrays (`display` for formatted text) |
| `write_range` | a 2D array from the top-left cell; strings starting with `=` are formulas, other strings are read as typed in a cell, numbers/booleans as given, null clears; formats are kept; all or nothing; returns the calculated values |
| `evaluate` | a formula against the workbook with the app's engine, nothing changed |
| `export` | `.xlsx`, or `.csv`/`.tsv` of one sheet, to a local path (no overwrite unless asked) |
| `join_session`, `leave_session` | stay connected as an agent (people see it in presence, with the range it works on; edits appear live); leave saves a version |
| `who_is_here` | the people in the workbook and the range each works on |
| `list_versions` | the version history |

Without `join_session` each call joins for the moment it takes (so it sees what was edited live)
and a write saves a snapshot, which is one version per write: join first when making many edits.
The agent's presence follows `read_range`/`write_range`/`join_session` (the range it touched).
An example exchange:

```
-> {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write_range",
    "arguments":{"item":"<id>","range":"H1","values":[[1,2],["=H1+I1",null]]}}}
<- {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"values\":[[1.0,2.0],[3.0,null]], ...}"}],"isError":false}}
```

CLI examples (the same operations, for scripts and shells):

```
fsheets login --server https://your-server.example:8090 --user agent-claude --password-file ~/agent-claude.pw
fsheets ls                                   # find folder and file ids
fsheets new --folder <folder-id> --name Forecast --sheets Data,Notes
fsheets write --item <id> --sheet Data --range A1 --values '[["Item","Q1"],["Rent",1000],["Total","=SUM(B2:B2)"]]'
fsheets cat --item <id> --sheet Data         # calculated values; --formulas for the formulas
fsheets eval --item <id> --sheet Data "=AVERAGE(B2:B9)"
fsheets export --item <id> --out /tmp/forecast.csv --sheet Data
fsheets versions --item <id>
```

## Soak harness

N clients (threads, one account each, cycled) edit the top-left 12x6 cells (so they collide),
insert and delete rows and columns, send presence, and drop their connection at random
(3% per action) and come back 0.1-0.6 s later while still editing offline. At the end every
client must be synced to the same seq, and a freshly joined client (which replays the whole log
from the file's version) must hold the same cell map. Pace is 40 ms per action because the
server allows about 30 batches/s per connection (closes with 4429 beyond that).

Test: `cargo test -p freedomsheets-agents` (server in-process on 127.0.0.1:0, temp data dir).

Runs on this branch:

| Where | Clients | Result |
|---|---|---|
| in-process test, seed 0x5eed | 3 x 80 actions | 211 ops in 138 batches, 138 acks, 276 remote batches applied, 5 forced disconnects/5 reconnects, converged yes (fresh joiner too), 3.5 s |
| shared test instance (Tailscale, TLS), Budget.xlsx | 4 x 200 actions | 732 ops in 607 batches, 605 acks, 1821 remote batches applied, 18 forced disconnects/18 reconnects, final seq 607, 173 cells, converged yes (fresh joiner too), 9.9 s |
| shared test instance, soak-m3.xlsx (M3: clients killed right after an edit and restarted from their journals) | 4 x 200 / 300 / 400 / 300 actions, kill chance 3 / 5 / 8 / 12 % per action | seeds 3001-3004: 734 to 1449 ops, 24 / 64 / 131 / 147 kills with 22 / 62 / 119 / 138 journals put back, 19 to 44 forced disconnects, 3 snapshots and (seed 3004) 3 reloads, up to 52013 remote batches applied; converged yes (fresh joiner too) and no journal left in all four, 11 to 23 s each |

## Shared test instance

`agents/scripts/shared-test-setup.sh` reproduces the instance (idempotent): data in
`~/freedomoffice-shared-test`, binds only the Tailscale address `100.x:8090` with the tailnet
certificate (`tailscale cert`), user unit `freedomoffice-sharedtest.service`, org TestOrg,
accounts testadmin (org admin), test-a..test-d and agent-claude (members), passwords from
`openssl rand` in `secrets/<user>.pw` (0600, read through stdin/files only), the three samples
(`Budget.xlsx`, `Tasks.xlsx`, `Grades.xlsx`) in TestOrg's folder. It backs the data up before
replacing the binary and revokes the seeding token.

## Formulas and .xlsx/.csv export with FreedomSheets' own engine (investigation)

**Done in M1b** (steps 1 to 4 below are implemented; the text is the investigation that led to it).
`freedom-sheets` has a lib target `freedom_sheets` with all non-UI modules; `app`, `crash` and
`icon` stay in the binary, which needs the default `ui` feature (eframe, rfd, image, winit are
optional dependencies behind it, and `live::conn` is gated by it). `fsheets` depends on it with
`default-features = false`. Deviations from the plan: `cloud` moved into the lib too (so
`pct_encode` needed no move), `io::load_xlsx_bytes`/`io::xlsx_bytes` and `pivot::sample` (test
data the app's tests share) were added, and a few `pub(crate)` items became `pub`.

Finding: **`sheets` can get a `[lib]` target without egui.** Only `app/` (35 files),
`live/conn.rs` and `main.rs` use egui/eframe. I tried it: a `sheets/src/lib.rs` declaring
`chart, condfmt, filter, formula, hash, iconset, io, live, model, numfmt, page, pivot,
sparkline, structure, table, validation, whatif` compiles (`cargo check --lib`) with exactly one
change: `live/conn.rs` (egui `Context`, `crate::cloud::pct_encode`) gated behind a feature (or
moved out). I reverted the experiment; nothing of it is committed.

What the agent gets from it:
- `io::xlsx_read::read` / `io::xlsx_write::write`, `io::delimited` (csv/tsv read and write): export and import identical to the app.
- `formula::recalc`, `eval_formula`, `eval_expr`: the app's results for every function it supports.
- `live::{assign_ids, join_workbook(file, state), decode_state, apply_ops, state_ops, encode_state, compare, contents}`: a real `Workbook` joined with the file *and the lossless live state*, remote ops applied exactly as the app does (with the `adj` formula adjustments, styles, merges, notes that `fsheets::book` skips), then recalculated.
- Consequence: an agent can also answer the server's `Snapshot` request (upload workbook + state through `/live/snapshot`), which `fsheets` cannot do today.

Recommended approach (M1b):
1. Add `sheets/src/lib.rs` (package `freedom-sheets`, lib name `freedom_sheets`) re-exporting the headless modules; in `main.rs` replace `mod x;` by `use freedom_sheets::{x, ...};` so every `crate::x` path in `app/` keeps working. Gate `live::conn` behind `feature = "ui"` (default on for the app; `fsheets` depends with `default-features = false`). Platform bits already `cfg`: `page.rs` uses `windows-sys` only on Windows.
2. Move `pct_encode` out of `cloud` (live/conn and crash use it).
3. Keep `eframe`, `rfd`, `image` and winit out of the lib's dependencies (optional deps behind `ui`) so `fsheets` doesn't pull in the GUI stack.
4. Replace `fsheets::book::Book` by `freedom_sheets::model::Workbook` plus `live::apply_ops`; keep `Book` only as the cheap read model if wanted. Add `fsheets eval "=SUM(A1:A5)"`, `export --format xlsx|csv`, `snapshot`.
5. Fallbacks if the lib proves too invasive later: ask a live app peer (no protocol for it), or read cached values (formula cells carry no values on the wire; the file's cached values go stale).

Before M1b `fsheets cat` printed formula text and did not calculate; it now prints calculated values.

## Gap audit vs Excel Online

Evidence is the harness (`probe`, `soak`) and the code on this branch.

| Area | State | Evidence |
|---|---|---|
| Presence / cursors | **Done**, minus a few things. Selection, active cell, "editing" flag, colour slot and names are relayed (rate limit 15/s) and painted by the app (coloured borders, name flag for a few seconds). Agents appear with `agent: true` but the app does not yet show that (no robot badge). No cursor for a cell being edited inside the formula bar, no following ("jump to user"), presence is per sheet only. | `cosync::Presence`, `app/live_ui.rs::paint_presence`, test `agents_are_marked_and_see_each_other` |
| Cell conflicts | **Done (M2, DECISIONS #12-15).** A cell is two targets: content (value/formula/rich text) and format; a value edit and a format edit of one cell made at the same time both survive (`fsheets probe`: value kept: true, format kept: true), two value edits or two format edits are last-writer-wins and the loser is told ("Your change to B3 was replaced by Alice's" in the app; `fsheets` prints it, MCP `write_range` / `replaced_changes` return it). Not done: per-attribute format merge (bold vs fill by two people at once keeps only the later style). | `cosync` `Op::Content` / `Op::Format`, `soak` (format edits in the random actions), `probe` |
| Offline / reconnect | **Done (M3, DECISIONS #17-21).** Reconnect resumes from the last seq, resends the unacknowledged batch, duplicates are recognised by (cid, batch id). Unsent changes are kept in a journal (`cosync::journal`: atomic, 0600, no credentials, written with every change, removed when acknowledged) so closing or killing the app or an agent, offline or mid-send, loses nothing: the next start reopens the document, puts the changes back when the replay reaches their seq and sends them once ("3 changes made offline were saved to the server"). Lost access keeps them as a copy file (app: opened, or Save a copy). A long offline period (server replies `reload`) keeps every change (test with 40 + 40 changes, structural ones included). Status bar: "Offline — changes will be sent when you're back online (N waiting)", countdown, editing never blocked. The soak kills and restarts clients from their journals (`--restart-pct`): 12 seeds locally and the run on the test instance converge, no journal left. Not done: conflicts after a `reload` are not shown (changes are re-based, not transformed); a batch in doubt whose commit is older than a newer snapshot can apply twice; the cell being typed in is not in the journal until committed. | `cosync/src/journal.rs`, `app/live_ui.rs` (`journal_save`, `live_restore_due`, `poll_journal_resume`), `agents/src/live.rs`, `soak.rs` |
| Per-user undo under concurrency | **Done.** Undo is per person: a stack of inverse ops carried over other people's changes (`cosync::carry_down`), and a step is blocked when someone else changed what it would restore (like Excel). Redo likewise, 100 steps. Not covered by the headless harness (no undo in `fsheets`). | `app/live_ui.rs` (`Undo`, `live_undo`, `carry_down` at line ~1084) |
| Autosave | **Done for agents (M1b); was partly.** `fsheets` now answers the server's `Snapshot` request like the app (soak test `clients_answer_the_servers_snapshot_requests`: the server asks past 200 batches, a client uploads, the file gets a version, a fresh joiner is identical) and snapshots when a one-shot command or `leave_session` ends with unsaved changes. Earlier finding: **Partly.** Committed batches are stored in SQLite at once (nothing is lost on a server restart); the server asks one editor for a snapshot every 200 batches / 120 s of changes / after a pause, and that becomes a version. Without a GUI editor nobody answers: in the soak the server asked once and `fsheets` ignored it, so the saved *file* stays at the old version while the log grows (a new joiner replays the log, so nothing is lost, but downloads and the web preview show the old file). Fixed by the lib target above. | `server/src/live.rs` (`SNAPSHOT_*`, `maybe_ask_snapshot`), soak report "snapshot requests ignored" |
| Version history | **Done server side**: every snapshot/save is a version (newest 25 plus the first of each day), restore and download exist; live restore is refused while a session runs (423). Gap: no per-user attribution inside a snapshot (a version is "modified by" the snapshotting client), and no "what changed" view, which Excel Online has. | `server/src/api.rs` versions endpoints, `live_busy` |
| Comments | **Gap.** Only plain notes (`Op::Note` with text and author) exist; no threads, replies, resolve, @mentions or notifications. Planned as a new op kind (DECISIONS #8). | `cosync::Op::Note` |
| Tracked by `fsheets` | **Everything the app synchronises** (M1b): formats, merges, notes, widths, features, `adj` and the lossless live state at join; the soak compares clients with `freedom_sheets::live::compare`. Earlier: only values and formulas. | `live.rs`, `soak.rs` |

Other findings: the live WebSocket authenticates with the bearer token header (the ticket
endpoint is for browsers); the server's rate limit (30 batches/s, burst 120) closes a client
that exceeds it with 4429, so an agent must pace itself; the first reconnect needs no `v`
(`Live` sends it only on the first join, otherwise a changed version would force a reload).
