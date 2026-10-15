# AGENTS.md

Guidance for AI coding agents (and humans skimming for conventions) working in
this repository. Keep this file in sync when architecture or workflows change.

## What this is

**Periscrawl** — a fast, local-first SEO crawler with three frontends sharing
one engine: a scriptable CLI, an egui desktop GUI, and a ratatui terminal UI.
All state lives in a local SQLite database; a loopback TCP sync layer lets the
CLI attach to a GUI-hosted crawl (start/stop/status/watch), and an MCP server
(`periscrawl mcp`) exposes crawl control to AI agents.

It is a from-scratch implementation; the GPL-3.0 reference app
(`fmfg03/RustySEO`) is design inspiration only — **never copy code from it**.

## Workspace layout

```
crates/
  periscrawl-core   engine: crawler, persistence, settings, analysis. NO UI deps (golden rule).
  periscrawl-cli    `periscrawl` binary — scriptable / agent-friendly (+ `mcp` mode)
  periscrawl-gui    `periscrawl-gui` binary — egui desktop app
  periscrawl-tui    `periscrawl-tui` binary — ratatui terminal app (legacy, rebased on core)
scripts/            packaging + icon generator (pure Python, no deps)
packaging/          installer sources (WiX/NSIS/AppRun/desktop entry)
docs/ROADMAP.md     architecture decisions + phase history — update it with behavior changes
```

## Commands

```bash
cargo build --workspace              # build everything (debug)
cargo test --workspace               # unit + fixture tests (fast, must stay green)
cargo build --release -p periscrawl-cli -p periscrawl-gui

# Packaging (unsigned, local):
./scripts/package-macos.sh           # .app + .zip + .dmg
./scripts/package-linux.sh           # tar.gz (+ .AppImage when appimagetool is present)
pwsh -File scripts/package-windows.ps1   # zip (+ .msi/.exe when WiX/NSIS are present)

# Manual smoke crawl against a local fixture (see "Verifying changes")
./target/debug/periscrawl crawl http://127.0.0.1:8931/ --max-pages 0 --local --no-db --quiet
```

## Golden rules

1. **`periscrawl-core` never depends on UI crates** (ratatui/crossterm/egui). The
   CLI, GUI and TUI are thin layers over it.
2. **One analyzer entry point**: `IssueAnalyzer::analyze_session(pages, base_domain)`.
   CLI, GUI, exports, PDF and MCP all consume it — do not run individual
   analyzers from frontends. Content issues are gated to 2xx HTML pages; status
   analyzers see everything. Any new analyzer must be registered inside
   `run_all_issues` or it will silently never run (this happened before — see
   the 7 dormant analyzers in the ROADMAP).
3. **Exports come from core sheet builders** (`export::build_report_sheets`).
   Frontends only choose formats/paths; never build report tables in UI code.
4. **Measurement integrity**: lengths are character counts, word counts read
   visible body text only (see `helpers/text.rs`), `alt=""` is valid, sizes are
   response bytes. Don't reintroduce byte-length or raw-DOM text counting.
5. **Platform artifacts are not content**: `/cdn-cgi/` URLs (Cloudflare email
   protection etc.) must never be crawled, flagged or listed —
   `url_normalizer::is_platform_artifact`.
6. **PSI is sampled and asynchronous**: sampling = seed-page nav + one page per
   URL template, capped by `psi::DEFAULT_SAMPLE_BUDGET`; actual calls run in a
   background permit pool (`DEFAULT_CONCURRENCY`, 3) writing through
   `db::update_page_cwv`. Never call PageSpeed inline in the fetch path again —
   it blocks crawl slots and that was a real performance bug.
7. **AI connectors were deliberately removed**. Do not re-add Gemini/OpenAI
   settings or chat UIs; AI analysis goes through the GUI's "Export prompt (AI)"
   flow (CSV to ~/Documents + clipboard prompt).
8. **DB schema changes need a migration** (`PRAGMA user_version` in `db.rs`);
   sessions must survive. `--max-pages 0` means unlimited, everywhere.
9. **Env hooks for automation are fine** (e.g. `PERISCRAWL_GUI_OPEN_SESSION`,
   `PERISCRAWL_GUI_EXPORT_AI`), but keep them one-shot and side-effect free
   unless explicitly requested.

## Where things live (start here)

| Area | File |
|---|---|
| Crawl loop, budgets, stop flag, PSI drain | `core/src/crawler/engine.rs` |
| Fetching, redirects, JS fallback, PSI workers | `core/src/crawler/fetching.rs` |
| HTML extraction (`PageData`), nav links, images, anchors | `core/src/crawler/helpers/html_parser.rs` |
| 40+ issue analyzers + gating + severity | `core/src/helpers/issues.rs` |
| PSI sampler (nav seed, template groups, budget) | `core/src/crawler/helpers/psi.rs` |
| URL normalization, crawl filter, artifact filter | `core/src/crawler/url_normalizer.rs` |
| DB schema, sessions, page storage, CWV updates | `core/src/db.rs` |
| Data dir + rename migration | `core/src/paths.rs` |
| Sheets (xlsx/csv/json) + severity tabs | `core/src/export.rs` |
| PDF report | `core/src/report.rs` |
| Log analyzer / sitemap validator / content audit | `core/src/{loganalyzer,sitemap,content_audit}.rs` |
| Sync server protocol + service | `core/src/server/mod.rs` |
| CLI commands, exit codes, session output | `cli/src/main.rs` |
| MCP tool surface | `cli/src/mcp.rs` |
| GUI state, crawl orchestration, exports | `gui/src/app.rs` |
| GUI rendering (tables, rings, panels, dialogs) | `gui/src/ui.rs` |

## Verifying changes

- `cargo test --workspace` must pass (currently ~57 tests; add coverage for
  behavior changes — psi.rs, url_normalizer.rs and issues.rs have good
  examples of targeted unit tests).
- For crawler/analyzer changes, run a real crawl against a **local fixture
  server** (a tiny Python `http.server` that links N pages, optionally with
  deliberate delays/404s). This is how stop-immediacy, parallel crawls, resume
  and sampling were validated. Never point tests at third-party sites.
- GUI changes: the app auto-loads a session via
  `PERISCRAWL_GUI_OPEN_SESSION=<id>`; screenshots via `screencapture -l <window
  id>` are enough to validate rendering. Don't restart a GUI instance the user
  says they are using.
- Exports: `periscrawl export <id> --format json` and inspect sheet names/rows.
- The stored database for the developer machine lives in
  `~/Library/Application Support/periscrawl` (macOS); treat it as user data —
  only touch it for explicitly requested cleanup.

## Conventions

- Exit codes: `0` ok, `1` fatal, `2` zero pages crawled, `3` config key
  missing, `6` regressions/sitemap findings, `7` watch timeout.
- `--json-events` emits NDJSON; the last record is always `crawl.end`.
- Commands never require a TTY; interactive confirmations need `--yes` for
  non-interactive use.
- Commit style: `feat(area): ...` / `fix(area): ...` with a body explaining
  measurements/verifications when behavior changed.
- Keep `docs/ROADMAP.md`'s phase list and known-issues current; log honest
  caveats there instead of leaving them in commit messages only.

## CI / releases

`.github/workflows/ci.yml` runs the test suite on pushes/PRs.
`.github/workflows/release.yml` (tags `v*` or manual dispatch) builds unsigned
installers on all three platforms and attaches them to a GitHub Release:
`.dmg` (macOS, universal2), `.msi` + `-setup.exe` (Windows, WiX/NSIS),
`.AppImage` + tar.gz (Linux). Version comes from the tag (`v0.3.0` → `0.3.0`)
or the manual-dispatch input.
