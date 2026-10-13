# Periscrawl

<div align="center">

<img src="docs/assets/logo.png" width="160" alt="Periscrawl logo">

**See everything on a site — a fast, local SEO crawler with a scriptable CLI,
a desktop GUI and a terminal UI, built in Rust.**

</div>

---

Periscrawl crawls a website breadth-first — respecting robots.txt, rate limits
and your page budget — then audits every page for technical SEO problems and
hands you the results as tables, spreadsheets, JSON, PDF reports, or a live
sync surface any frontend (or AI agent) can attach to.

## Highlights

- **Real crawl engine** — concurrent BFS, robots.txt enforcement, adaptive
  pacing (429/5xx aware), gzip/deflate, retries, redirect-chain tracking,
  optional headless-Chrome rendering for client-side pages (byte-identical
  detection heuristics keep it off when not needed).
- **Incremental re-crawls** — `--since <session>` uses ETag / Last-Modified so
  unchanged pages answer `304` and are reused instead of re-downloaded.
- **Resume & no-limit crawls** — `--resume <session>` continues an interrupted
  run; `--max-pages 0` crawls until the frontier is empty or you stop it
  (`periscrawl stop`, or the GUI's Stop button).
- **36+ issue analyzers** with consistent gating: content checks only look at
  `2xx` HTML pages (never 404 boilerplate or binary files), and a single
  session-level entry point feeds the CLI, GUI, exports, PDF and MCP so every
  surface reports identical numbers.
- **Whole-crawl link-graph findings** — broken internal links
  (`source → target (status)`), orphan pages, redirect chains, redirects that
  land on errors, internal `nofollow`, canonical-to-broken targets.
- **Measurement you can trust** — title/H1/description lengths are character
  counts (correct for Cyrillic and all non-Latin text), word counts and
  keyword extraction read only visible body text (scripts/RSC payloads
  excluded), `alt=""` is treated as valid (decorative) rather than missing,
  and `Size (bytes)` is the actual response body size.
- **Core Web Vitals without the API bill** — optional PageSpeed Insights
  integration samples smartly: every unique header/nav/footer link, then one
  representative page per URL template (`/news/*`, `/projects/*`, …). A
  3,000-page audit costs ~40–60 PSI calls instead of 6,000.
- **Local-first, agent-friendly** — everything lives in a local SQLite
  database; the CLI emits NDJSON event streams and documented exit codes, and
  `periscrawl mcp` exposes crawl control + session queries over MCP stdio.

## Getting started

### Build from source

```bash
cargo build --release
# binaries: target/release/periscrawl, periscrawl-gui, periscrawl-tui
```

Requires a Rust toolchain (stable). Headless Chrome is optional — only needed
for `--js` / JS-fallback rendering.

### CLI

```bash
# Crawl (delegates to a running GUI/server when one is up; --local opts out)
periscrawl crawl https://example.com --max-pages 300

# Unlimited crawl until stopped or every link is followed
periscrawl crawl https://example.com --max-pages 0
periscrawl stop

# Investigate a session
periscrawl sessions
periscrawl session 1 show
periscrawl session 1 issues --max-urls 5

# Incremental re-crawl + regression check (exit code 6 on regressions)
periscrawl crawl https://example.com --since 1 --compare 1

# Exports: xlsx (all sheets), csv, json, client-facing pdf
periscrawl export 1 --format xlsx
periscrawl export 1 --format pdf

# Attach/automate
periscrawl serve            # headless crawl server (CLI clients attach)
periscrawl status | watch | stop
periscrawl mcp              # MCP server on stdio for AI agents
```

Distinct exit codes make it automation-safe: `0` ok, `1` fatal, `2` zero
pages crawled, `3` config key missing, `6` regressions/sitemap findings,
`7` watch timeout.

### Desktop GUI

```bash
periscrawl-gui
```

Sessions browser, crawl form (including “0 = unlimited”), live crawl view with
Stop, sortable/filterable tables per sheet, page/asset/issue/diff detail
panes, settings & connectors, and one-click exports (XLSX / CSV / JSON /
client-facing PDF) that reveal the file when done. The GUI hosts the sync
server, so `periscrawl status` / `periscrawl crawl` from any terminal attach to
the crawl that is already running.

### Terminal UI

```bash
periscrawl-tui
```

The classic ratatui experience, sharing the same engine and sessions.

### Packaged apps

```bash
./scripts/package-macos.sh      # dist/Periscrawl.app (signed ad-hoc) + zip
./scripts/package-linux.sh      # tar.gz + install.sh + .desktop entry
./scripts/package-windows.ps1   # zip with exe + start menu hint
```

## Configuration

Settings live in the platform data directory:

- **macOS**: `~/Library/Application Support/periscrawl/cli-settings.toml`
- **Linux**: `~/.local/share/periscrawl/cli-settings.toml`
- **Windows**: `%APPDATA%\periscrawl\data\cli-settings.toml`

```bash
periscrawl config show                 # the full settings file
periscrawl config set crawler.concurrency 20
periscrawl config set connectors.pagespeed.api_key <KEY>
periscrawl config set connectors.pagespeed.status true
```

The PageSpeed Insights key (Google Cloud Console → enable the *PageSpeed
Insights API* → create an API key) unlocks the CWV columns, the “Slow Load”
issue and the CWV sheet. Sampling keeps it cheap; leave the toggle off for
full-site crawls you don't need CWV for.

Sessions, pages, exports, logs and the sync token all live next to the
settings file, never in the cloud.

## Issue analysis

Analyzers cover: HTTP status codes (4xx/5xx/3xx), titles/descriptions/H1s
(missing, duplicated, wrong length — character-accurate), thin content, large
pages, canonicals (multiple / elsewhere / broken targets), indexability
(meta robots and X-Robots-Tag), mixed content, redirect behavior, URL quality
(uppercase paths, underscores, non-ASCII, length, parameters), images
(missing alt with `alt=""` semantics, oversized, non-WebP), internal link
graph (broken targets, orphans, nofollow, low link counts), anchors
(generic/empty — with `aria-label` / image-alt fallbacks), CWV performance,
analytics/id presence, and sitemap/robots hygiene.

## Architecture

```
crates/
  periscrawl-core   engine: crawler, persistence, settings, analysis. No UI deps.
  periscrawl-cli    `periscrawl` binary — scriptable / agent-friendly
  periscrawl-gui    `periscrawl-gui` binary — egui desktop app
  periscrawl-tui    `periscrawl-tui` binary — ratatui terminal app
```

All state lives in SQLite (WAL) in the platform data directory so any
frontend can open any crawl session; a loopback socket (`sync.token`-authed)
adds live control and crawl delegation. Details and phase history in
[docs/ROADMAP.md](docs/ROADMAP.md).

## Development

```bash
cargo test --workspace     # unit + fixture tests
cargo build --workspace
```

Contributions welcome — keep `periscrawl-core` UI-free, keep analyzer gating
in the shared runner, and add tests with behavior changes.

## Privacy

Crawling and analysis are fully local. Network calls happen only to the site
being crawled, and — only when you configure the keys — to Google's PageSpeed
Insights API and your chosen AI provider.

## License

No license file has been added to this repository yet; until one is, all
rights are reserved by the author. Add a `LICENSE` before distributing.

## Acknowledgments

- [mascanho/RustySEO-Headless](https://github.com/mascanho/RustySEO-Headless) —
  the upstream project this repository forked from.
- [fmfg03/RustySEO](https://github.com/fmfg03/RustySEO) (GPL-3.0) — design
  reference; features were reimplemented, not copied.
- Built with [scraper](https://crates.io/crates/scraper),
  [ratatui](https://crates.io/crates/ratatui),
  [egui/eframe](https://crates.io/crates/eframe) and
  [axum](https://crates.io/crates/axum).
