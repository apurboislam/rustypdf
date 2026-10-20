# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

rustypdf is a Rust HTTP service, run in Docker. It renders Typst templates
into **PDF/A-3b** documents from JSON sent to the API.

Design docs:
- `PLAN.md` says **what** to build and why (the source of truth for decisions);
- `BUILD.md` says **in what order**, as phases P0–P9, each with a task checklist and "Done when" criteria.

P0–P8 are done (render core, Bangla + English, HTTP API, static images +
manifest + schema, URL images, Docker + ops, sector invoice templates,
performance). P9 (extras) is optional; tick its checkboxes as tasks land.
How to write a template: `docs/templates.md`. Speed and memory numbers and
how to measure them: `docs/performance.md`. When a decision
changes, update `PLAN.md` in the same change.

## Commands (`justfile`)

```bash
just fmt / just lint / just test / just run / just docker
cargo test <test_name>                      # single test
cargo test --test <file> <test_name>        # single integration test (tests/<file>.rs: api, images, ops, render, golden)
just golden-update                          # re-approve golden PNGs (only on purpose, give a reason in the PR)
just pdfa                                   # render every templates/*/sample.json (photos from a mock server) + veraPDF 3b
cargo run -- render <template-dir> <data.json> -o out.pdf   # dev CLI: same pipeline as the service, no HTTP
#   add --resolve cdn.rustypdf.test=127.0.0.1:PORT to serve sample photo URLs from a local server
scripts/verapdf.sh out.pdf                  # veraPDF 3b (PATH verapdf, else fetched via mvn)
just bench / just soak / just loadtest      # criterion; in-process soak; k6 against the image
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy
--all-targets -D warnings`, `cargo test` (incl. golden PNGs), `just pdfa`,
builds the Docker image and smoke-tests it (`--read-only`, nonroot), and
runs the criterion benchmarks (`scripts/bench-ci.sh`: warns, never fails,
on a >20% regression against the last `main` run).
Without env vars, `TEMPLATES_DIR`/`FONTS_DIR` fall back to `./templates` and
`./fonts` when `/templates` and `/fonts` don't exist.

Tests: `tests/common/mod.rs` has shared helpers (fonts loaded once, fixed
timestamp, `state_with`/`state_with_mock`, `send`). `tests/fixtures/templates/`
holds API-test templates: `greet`, `broken`, `slow` (loop count from `data.n`,
so tests avoid comemo cache hits), `missing-asset`, `bad-manifest` (skipped at
load) and `photo-test` (route `photos`: logo, manifest image fields, schema,
sample). URL-image tests use `wiremock` plus `Fetcher::with_test_overrides`,
which maps only the host `img.test` to the mock server; every other host,
including `localhost` and IP literals, stays SSRF-guarded.
`tests/golden.rs` renders every repo template's sample through the full
pipeline (photo URLs served from `tests/fixtures/photos`) and fails if a
template with a `sample.json` has no golden test.

## Architecture (big picture)

Typst is embedded **as a library**, not called as the CLI. Speed comes from
doing the expensive work once and sharing it:
- fonts are loaded once into `Arc<FontStore>`;
- template sources and assets are cached in memory;
- Typst's `comemo` memoization carries over between requests, evicted every
  `MEMO_EVICT_EVERY` renders (`src/memo.rs`). Never go back to timer-only
  eviction: the cache grows by about 1.7 MB per render.

Per request, the only new inputs are the request's JSON and its images.

Request flow (`src/api/render.rs`, then `images/`, then `render.rs`):
1. Request ID + access log + metrics (`observe` middleware), then auth
   (`API_KEYS`; `/healthz` and `/readyz` stay open), then the body-size limit
   (a manifest's `max_body_kb` overrides `MAX_BODY_KB`).
2. Validate the JSON against the template's `schema.json`. Then NFC-normalize
   all strings.
3. **Resolve images** (`src/images/`), in parallel. For each field listed under
   `[images."<json-pointer>"]` in `manifest.toml` (`[]` means every array item),
   the value's form decides how it loads:
   - `http(s)` URL: fetched, SSRF-guarded, LRU-cached (moka) and resized to `max_px`;
   - `data:` URI: decoded inline;
   - file path: read from the preloaded template or `_shared` assets;
   - empty: the field's `fallback` file.

   Each result becomes a virtual file `/__img/<sha256>.<ext>`, and the JSON
   value is rewritten to that path. **This happens before acquiring a compile
   slot**, so slow networks never block CPU workers.
4. Acquire the `Semaphore` (`MAX_CONCURRENCY`), then `spawn_blocking`. Build a
   `RenderWorld` (`src/world.rs`, which implements `typst::World`), run
   `typst::compile`, then `typst_pdf::pdf` with `PdfStandard::A_3b`.

`RenderWorld` serves:
- `/data.json`: the rewritten request JSON. Templates read it with `json("/data.json")`;
- `/__img/*`: the request's resolved images;
- everything else: from the template-root cache.

The Typst project root is `/templates`, so templates can import
`/_shared/...`. File access outside it is rejected. Templates never touch the
network; only `images/fetch.rs` does.

`TemplateRegistry` (`src/registry.rs`) is an `ArcSwap<HashMap>` keyed by route:
- every folder in `TEMPLATES_DIR` that contains `main.typ` becomes a template
  served at `POST /render/{route}` (the folder name unless `manifest.toml`
  sets `route`). Folders starting with `_` are not templates. A template with
  an invalid `manifest.toml`/`schema.json`, a bad fallback, or a duplicate
  route is skipped with an error log;
- hot reload (`notify`) rebuilds the registry and swaps it atomically. A
  template that breaks on reload keeps its previous version.

## Invariants that are easy to break

- **Every output is PDF/A-3b.** Do not add an option to turn this off, and do
  not add PDF attachments; the output is the PDF only.
- **Fonts:** Bangla uses Noto Sans Bengali and everything else uses Inter.
  This is enforced with Typst's font `covers` regex
  (`U+0980–U+09FF` + ZWNJ/ZWJ) in `templates/_shared/lang.typ`.
  `REQUIRED_FONTS` missing at startup means the service refuses to start;
  keep it that way, never silently fall back to another font.
- **Digits are never converted.** Bangla digits (০–৯) and ASCII digits print
  exactly as received. For computed values, templates use `num()` to parse and
  `digits-like(value, input)` to print in the same digit system as the input.
- **Images are hybrid:**
  - static images (logos, stamps) are files referenced literally in the
    template (`#image("assets/logo.png")`) and preloaded at startup;
  - dynamic images (product photos) are URLs in the request, declared in the
    manifest.

  Templates use the same `image(...)` call either way.
- **Image fetch safety:**
  - the SSRF guard (block private, loopback, link-local and metadata IPs,
    re-checked on every redirect) always applies;
  - `IMAGE_ALLOWED_HOSTS` is optional: comma-separated, supports a `*.`
    wildcard, and allows any public host when unset.
- `typst`, `typst-pdf` and `typst-render` are pinned to an exact version. The
  `World` and PDF APIs change between releases, so any upgrade must re-run the
  golden and veraPDF checks.
- Never loosen a failing veraPDF, golden-image or SSRF test to get CI green.
