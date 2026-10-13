# RustySEO — Architecture & Roadmap

RustySEO is an open source alternative to commercial SEO crawlers (Screaming Frog
et al.). This repository contains the crawl engine plus three frontends sharing
it: a scriptable CLI, a desktop GUI, and a terminal UI.

## Workspace layout

```
crates/
  rustyseo-core   # engine: crawler, persistence, settings, issue analysis. No UI deps.
  rustyseo-cli    # `rustyseo` binary — scriptable / agent-friendly
  rustyseo-gui    # `rustyseo-gui` binary — egui desktop app (Phase 3)
  rustyseo-tui    # `rustyseo-tui` binary — ratatui terminal app (was RustySEO-CLI)
```

Rules:

- `rustyseo-core` must never depend on ratatui, crossterm, egui, or similar.
- All state lives in SQLite (WAL) at the platform data dir, so any frontend can
  open any crawl session. A local socket sync layer (Phase 4) adds live control.

## Decisions (locked)

| Topic | Decision |
|---|---|
| GUI stack | egui / eframe (pure Rust, single binary, cross-platform) |
| CLI ↔ GUI sync | Shared SQLite (WAL) + local socket for live attach |
| TUI | Kept as a workspace member, rebased on core |
| Licensing | Reimplement inspiration from the GPL-3.0 reference app; no code copied |
| AI chat | Not in v1 scope (code remains, no investment) |

## Phases

### Phase 1 — Core split + CLI foundations (complete)

- [x] Cargo workspace; extract `rustyseo-core` (crawler, db, settings, issues, ai, logging)
- [x] `rustyseo` CLI v1: `crawl`, `config get/set/show/path/edit`, `doctor`
- [x] NDJSON event stream (`--json-events`), documented exit codes
- [x] Session schema + migrations (`sessions` table, per-session pages, `PRAGMA user_version`)
- [x] robots.txt enforcement + adaptive rate limiter (respect_robots / respect_crawl_delay settings)
- [x] Smart JS rendering fallback (render when client-framework markers meet sparse content)
- [x] `rustyseo sessions`, `rustyseo session <id> show|pages|issues` (human + `--json`)
- [x] `rustyseo export <session> --format json|xlsx|csv` (core sheet builders + writers; `--tab` / `--out -` for agents)
- [x] Precise `--max-pages` semantics (spawn-gated budget, no overshoot)

### Phase 2 — CLI v2 (complete)

- [x] JSON output everywhere with `--fields` projection; documented exit codes
- [x] `rustyseo crawl --resume <session>` (continue interrupted crawls; page ids continue safely)
- [x] Crawl diffing: `rustyseo diff a b`, plus `crawl --compare <baseline>` with
      regression exit code 6 (removed pages, new 4xx/5xx, newly noindexed)
- [x] Scheduled-automation ergonomics: `--quiet`, NDJSON events, regression gating
- [x] Incremental re-crawls: `crawl --since <session>` uses ETag / Last-Modified
      (304 → stored content reused, `not_modified` reported per page)

### Phase 3 — GUI (egui)

- [x] GUI crate: session browser, crawl form, live crawl view (progress + page stream),
      per-sheet data tables with text filtering, export buttons (xlsx / CSVs / json)
- [x] Horizontal scrolling for wide tables; clickable columns headers with sorting
- [x] Deduplicated asset sheets (Images/CSS/JS/Files list each asset once with
      "Pages Using" counts); where-used lists in the detail pane
- [x] CWV and Content tabs (new core sheet builders)
- [x] Page detail pane (headings, meta, canonicals, links, images, schema, extraction)
- [x] Issue audit with severities (critical/warning/notice) + full per-issue URL lists
- [x] Crawl diff view (compare two sessions: added/removed/changed pages + field diffs)
- [x] Settings & connectors screen (crawler defaults, robots/nofollow/noindex, API keys)
- [x] File logging for GUI runs (`.../rustyseo/gui.log`)
- [ ] Packaged app bundles (macOS .app / Windows .exe / Linux) with icons

### Phase 4 — Sync + agent-native (complete)

- [x] Loopback socket protocol (NDJSON over 127.0.0.1 TCP with `sync.token` auth)
- [x] Shared `CrawlService`: GUI hosts the sync server; `rustyseo serve` for headless;
      `rustyseo status` / `watch` / `stop` attach from any terminal
- [x] Crawl delegation: `rustyseo crawl` runs through the active server by default
      (`--local` opts out) so every frontend sees the same crawl live
- [x] Stop support end-to-end (engine stop flag, GUI Stop button, sessions marked `stopped`)
- [x] MCP server mode (`rustyseo mcp`): crawl control + session queries over stdio

### Phase 5 — Feature ports (from the pre-closed-source design)

- [x] Sitemap validator (`rustyseo sitemap`): HEAD-checks every sitemap entry with
      redirect-chain awareness; exit 6 when dead entries are found
- [x] Log analyzer (nginx/apache): `rustyseo logs <file>` — bot vs human traffic
      (search + AI crawlers classified), status/file-type breakdowns, top paths,
      error paths, top IPs and per-day crawl-waste views; `--json` for agents
- [ ] GUI surface for the log analyzer (tabs/charts)
- [x] PDF reporting: `rustyseo export <session> --format pdf` + GUI "Export PDF" —
      styled client-facing report (summary tiles, issues by severity, broken pages)
- [x] Keyword & content tooling: `rustyseo content <session> [--keywords a,b]`
      (word stats, thin pages, site-wide keywords, target checks) + Keywords sheet
- [ ] GSC / GA4 / Clarity connectors
- [x] Packaged app bundles: macOS `.app` (generated icon, ad-hoc signed, zip),
      Linux tar.gz + install.sh + .desktop, Windows zip — via `scripts/package-*`

## CLI conventions

- Exit codes: `0` ok, `1` fatal, `2` zero pages crawled, `3` config key missing.
- `--json-events` emits one JSON object per line; the final record is always
  `{"event":"crawl.end", ...}`.
- Commands never require a TTY; all interactive affordances are optional.

## Known issues being tracked

- `Ctrl+I` URL entry is unreachable on macOS/Linux (terminal sends plain `Tab`);
  needs a rebind in the TUI.
- Robots.txt is parsed but not enforced; `AdaptiveRateLimiter` is dead code.
- `max_pages` overshoots because in-flight requests are allowed to finish.
- Reference app (`fmfg03/RustySEO`, GPL-3.0) is design reference only.
