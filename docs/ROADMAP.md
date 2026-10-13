# Periscrawl — Architecture & Roadmap

Periscrawl is an open source alternative to commercial SEO crawlers (Screaming Frog
et al.). This repository contains the crawl engine plus three frontends sharing
it: a scriptable CLI, a desktop GUI, and a terminal UI.

## Workspace layout

```
crates/
  periscrawl-core   # engine: crawler, persistence, settings, issue analysis. No UI deps.
  periscrawl-cli    # `periscrawl` binary — scriptable / agent-friendly
  periscrawl-gui    # `periscrawl-gui` binary — egui desktop app (Phase 3)
  periscrawl-tui    # `periscrawl-tui` binary — ratatui terminal app (legacy TUI, rebased on core)
```

Rules:

- `periscrawl-core` must never depend on ratatui, crossterm, egui, or similar.
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

- [x] Cargo workspace; extract `periscrawl-core` (crawler, db, settings, issues, ai, logging)
- [x] `periscrawl` CLI v1: `crawl`, `config get/set/show/path/edit`, `doctor`
- [x] NDJSON event stream (`--json-events`), documented exit codes
- [x] Session schema + migrations (`sessions` table, per-session pages, `PRAGMA user_version`)
- [x] robots.txt enforcement + adaptive rate limiter (respect_robots / respect_crawl_delay settings)
- [x] Smart JS rendering fallback (render when client-framework markers meet sparse content)
- [x] `periscrawl sessions`, `periscrawl session <id> show|pages|issues` (human + `--json`)
- [x] `periscrawl export <session> --format json|xlsx|csv` (core sheet builders + writers; `--tab` / `--out -` for agents)
- [x] Precise `--max-pages` semantics (spawn-gated budget, no overshoot)

### Phase 2 — CLI v2 (complete)

- [x] JSON output everywhere with `--fields` projection; documented exit codes
- [x] `periscrawl crawl --resume <session>` (continue interrupted crawls; page ids continue safely)
- [x] Crawl diffing: `periscrawl diff a b`, plus `crawl --compare <baseline>` with
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
- [x] File logging for GUI runs (`.../periscrawl/gui.log`)
- [x] Packaged app bundles (macOS .app / Windows .exe / Linux) with icons (completed in Phase 5)

### Phase 4 — Sync + agent-native (complete)

- [x] Loopback socket protocol (NDJSON over 127.0.0.1 TCP with `sync.token` auth)
- [x] Shared `CrawlService`: GUI hosts the sync server; `periscrawl serve` for headless;
      `periscrawl status` / `watch` / `stop` attach from any terminal
- [x] Crawl delegation: `periscrawl crawl` runs through the active server by default
      (`--local` opts out) so every frontend sees the same crawl live
- [x] Stop support end-to-end (engine stop flag, GUI Stop button, sessions marked `stopped`)
- [x] MCP server mode (`periscrawl mcp`): crawl control + session queries over stdio

### Phase 5 — Feature ports (from the pre-closed-source design)

- [x] Sitemap validator (`periscrawl sitemap`): HEAD-checks every sitemap entry with
      redirect-chain awareness; exit 6 when dead entries are found
- [x] Log analyzer (nginx/apache): `periscrawl logs <file>` — bot vs human traffic
      (search + AI crawlers classified), status/file-type breakdowns, top paths,
      error paths, top IPs and per-day crawl-waste views; `--json` for agents
- [ ] GUI surface for the log analyzer (tabs/charts)
- [x] PDF reporting: `periscrawl export <session> --format pdf` + GUI "Export PDF" —
      styled client-facing report (summary tiles, issues by severity, broken pages)
- [x] Keyword & content tooling: `periscrawl content <session> [--keywords a,b]`
      (word stats, thin pages, site-wide keywords, target checks) + Keywords sheet
- [ ] GSC / GA4 / Clarity connectors
- [x] Packaged app bundles: macOS `.app` (generated icon, ad-hoc signed, zip),
      Linux tar.gz + install.sh + .desktop, Windows zip — via `scripts/package-*`

### Phase 6 — Rebrand & audit-integrity pass (complete)

- [x] Project renamed to **Periscrawl** with a new logo/icon set
      (`docs/assets/logo.png`, generated by `scripts/make_icon.py`); a one-time
      data-directory migration keeps existing sessions, settings and exports
- [x] Single session-level analyzer entry point (`IssueAnalyzer::analyze_session`)
      shared by CLI, GUI, exports, PDF and MCP; content issues only consider
      2xx HTML pages, so error-page boilerplate and binary files never leak in
- [x] Registered the whole-crawl link-graph analyzers: broken internal links
      (`source → target (status)`), orphan pages, redirect chains, redirects
      that land on errors, internal nofollow, canonical-to-broken targets
- [x] Measurement integrity: character-accurate title/H1/description lengths,
      visible-text word counts and keyword extraction (scripts/RSC excluded),
      real response body size, path-only uppercase-URL detection, `alt=""`
      treated as valid, aria-label / image-alt anchor-text fallbacks
- [x] PageSpeed Insights sampling (nav pages + one per URL template):
      a 300-page audit collects CWV with ~38 API calls instead of 600
- [x] `--max-pages 0` = crawl everything until stopped or the frontier is exhausted

## CLI conventions

- Exit codes: `0` ok, `1` fatal, `2` zero pages crawled, `3` config key missing.
- `--json-events` emits one JSON object per line; the final record is always
  `{"event":"crawl.end", ...}`.
- Commands never require a TTY; all interactive affordances are optional.

## Known issues being tracked

- `Ctrl+I` URL entry is unreachable on macOS/Linux (terminal sends plain `Tab`);
  needs a rebind in the TUI.
- Cloudflare `cdn-cgi/l/email-protection` obfuscation links (scraped from
  `mailto:` markup) inflate 404 / broken-link counts on Cloudflare sites;
  consider a crawl filter.
- PSI sampling uses the first crawled page as each template's representative;
  a "most linked / shortest URL" preference would pick better samples.
- Windows and Linux packaging scripts have not been exercised on real machines.
- Reference app (`fmfg03/RustySEO`, GPL-3.0) is design reference only.
