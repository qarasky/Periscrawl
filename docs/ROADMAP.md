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

### Phase 1 — Core split + CLI foundations (in progress)

- [x] Cargo workspace; extract `rustyseo-core` (crawler, db, settings, issues, ai, logging)
- [x] `rustyseo` CLI v1: `crawl`, `config get/set/show/path/edit`, `doctor`
- [x] NDJSON event stream (`--json-events`), documented exit codes
- [x] Session schema + migrations (`sessions` table, per-session pages, `PRAGMA user_version`)
- [x] `rustyseo sessions`, `rustyseo session <id> show|pages|issues` (human + `--json`)
- [x] `rustyseo export <session> --format json|xlsx|csv` (core sheet builders + writers; `--tab` / `--out -` for agents)
- [ ] Enforce robots.txt during crawling; wire the adaptive rate limiter
- [ ] Precise `--max-pages` semantics (currently overshoots by up to concurrency)

### Phase 2 — CLI v2 (agent surface)

- [ ] Stable JSON schemas for all commands; `--fields` projection
- [ ] `rustyseo crawl --resume`, incremental re-crawls, crawl diffing
- [ ] Scheduled-automation ergonomics (quiet mode, non-zero exit on regressions)

### Phase 3 — GUI (egui)

- [ ] Crawl dashboard: live progress, per-tab data tables (internal/external
      links, redirects, images, CSS/JS, content, CWV, files)
- [ ] Issue audit view with severity filtering
- [ ] Page detail pane, exports, crawl history + diff view
- [ ] PageSpeed Insights / CWV integration
- [ ] Settings & connectors screens

### Phase 4 — Sync + agent-native

- [ ] Local socket protocol (NDJSON over Unix socket / Windows named pipe)
- [ ] Live GUI ↔ CLI attach: start in one, watch/control from the other
- [ ] MCP server mode (`rustyseo mcp`) exposing crawl/query tools
- [ ] Optional `rustyseo serve` daemon

### Phase 5 — Feature ports (from the pre-closed-source design)

- [ ] Log analyzer (Nginx/Apache): bot vs human, status/filetype breakdowns,
      crawl-budget views
- [ ] GSC / GA4 / Clarity connectors
- [ ] Keyword & content tooling (tracking, topics, content planner)
- [ ] PDF reporting

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
