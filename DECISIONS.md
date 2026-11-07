# DECISIONS — branch collab-365 (for Petar's review; Guardian answers questions via outbox)

Started 2026-11-07 16:15 Dubai, from `orgs` at c0d9c74. Brief: inbox/20261107-1415-collab-365.md.

| # | Decision (reversible) | Why | Default if no answer |
|---|---|---|---|
| 1 | Drive D1c deploy on hold; drive-web review fixes finish on their branch only, no merge. | New priority. | Kept on hold. |
| 2 | Shared test folder = a **separate test server instance**, data dir `~/freedomoffice-shared-test`, `127.0.0.1:8090` (loopback only, no tailscale serve), built from collab-365. The production instance on :8080 is not touched. | "No deploys"; a test instance can be deleted without touching real data. | Same. Tailnet access for real GUI testers later only if Guardian OKs. |
| 3 | Test instance gets its own admin + accounts `test-a`..`test-d` in org "TestOrg", passwords generated into 0600 files inside the test folder (never argv/transcript). | Fresh data; no credential copying from production. | Same. |
| 4 | Multi-client tests are headless: a scripted client built on `cosync` (WebSocket) plus the app's existing in-process two-session tests. GUI runs only in Xephyr/Xvfb, never the20th :0. | Brief: headless/scripted. | Same. |
| 5 | collab-365 commits stay **local** (not pushed). | Guardian 12:05: no pushes without direct approval; repo is public. | Push when Guardian/Petar says. |
| 6 | Subagents run on Sonnet. | Brief: "Sonnet only". | Same. |
| 7 | Cell conflicts: keep last-writer-wins per target (as Excel Online does), but split a cell into value/formula and format so concurrent value + format edits both survive, and tell the user when their edit was replaced by someone else's. | Closest to 365 behaviour without a new data model. | Same. |
| 8 | Comments: threaded comments (replies, resolve) as a new op kind, separate from existing notes. | 365 has both notes and threaded comments. | Build after M1-M4. |

## Questions (default chosen, work continues)
- Q1: Open the test instance on the tailnet (tailscale serve on a second port) for Petar's own GUI testing? Default: no, loopback only.
- Q2: May collab-365 be pushed to origin? Default: no until approved.
