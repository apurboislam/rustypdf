# rustypdf — Build Phases

This file splits the work into phases, in build order. [`PLAN.md`](PLAN.md)
describes what the finished service does; this file says in what order to
build it. Each phase:
- leaves the project in a working, tested state;
- ends with a commit or PR;
- has a **Done when** list that must all be true before the next phase starts.

```
P0 Setup ─▶ P1 Render core ─▶ P2 Bangla+English ─▶ P3 HTTP API ─▶ P4 Static images + manifest
                                                                         │
                     P8 Performance ◀─ P7 Templates + CI gates ◀─ P6 Docker + ops ◀─ P5 Dynamic images
                            │
                            ▼
                      P9 Extras (optional)
```

| Phase | Name | Result | PLAN.md |
|---|---|---|---|
| P0 | Project setup | Empty service builds; CI runs | §2 |
| P1 | Render core | JSON + template → PDF/A-3b, as a library | §5, §8 |
| P2 | Bangla + English | Correct bilingual text with per-script fonts | §4 |
| P3 | HTTP API | `POST /render/{template}` works | §7, §8 |
| P4 | Static images + manifest | Logos from template folders; schema validation | §3, §6 |
| P5 | Dynamic images | Product photos from URLs, cached and resized | §6, §11 |
| P6 | Docker + ops | Production-ready container | §9, §10, §11 |
| P7 | Templates + CI gates | 4 sector invoices; veraPDF and golden tests in CI | §4, §5, §12 |
| P8 | Performance | Benchmarks meet targets | §2 |
| P9 | Extras | Batch, OpenAPI, client snippets | §12 M4 |

P0–P3 are the minimum usable service, with PDF/A-3b and Bangla working.
P4–P6 make it production-ready, and P7–P8 make it verified and fast.

---

## P0 — Project setup

**Goal:** an empty but well-structured Rust project with CI from day one.

**Tasks**
- [x] `cargo new rustypdf`, with edition 2024 and `rust-toolchain.toml` pinning a stable version.
- [x] Add dependencies, pinning an exact `typst` / `typst-pdf` / `typst-render`
      version because the Typst API changes between releases:
  - `axum`, `tokio`, `serde`, `serde_json`, `thiserror`, `anyhow`;
  - `tracing`, `tracing-subscriber`.
- [x] Create the module skeleton from PLAN.md §8 (empty modules, compiling).
- [x] Add `config.rs`, which reads the env vars from PLAN.md §9 with defaults.
- [x] Add `fonts/`:
  - Noto Sans Bengali and Inter, all weights;
  - their `OFL.txt` licenses;
  - `fonts/README.md` giving the source and version of each.
- [x] Add `templates/` with a minimal `hello/main.typ`, for tests.
- [x] Add a `justfile` (or `Makefile`) with `fmt`, `lint`, `test`, `run` and `docker`.
- [x] Add GitHub Actions CI running `cargo fmt --check`, `cargo clippy -D warnings` and `cargo test`.
- [x] Add `.gitignore`, `.dockerignore`, `LICENSE` and a short `README.md`.
- [x] `scripts/verapdf.sh` + `scripts/check-pdfa.sh` (`just pdfa`): veraPDF 3b
      on every `sample.json`, also run in CI (pulled forward from P7).

**Done when**
- `cargo build` and `cargo test` pass locally and in CI.
- `cargo run` starts, logs its config, and exits cleanly.

---

## P1 — Render core (library, no HTTP)

**Goal:** turn a template folder and a JSON value into PDF/A-3b bytes in
process. This is the heart of the project.

**Tasks**
- [x] `fonts.rs`:
  - load every font in `FONTS_DIR` once into `Arc<FontStore>` (a `FontBook` plus lazily parsed fonts);
  - fail fast if any of `REQUIRED_FONTS` is missing.
- [x] `world.rs`: implement `typst::World` for `RenderWorld`.
  - Shared parts: `Library`, `FontStore`, and the template file cache.
  - Per-request virtual files: `/data.json`, and `/__img/*` (a stub for now).
  - File access is sandboxed to the template root; reject `..` and paths outside it.
  - `today()` returns the current UTC date.
- [x] `render.rs`:
  - `render(template, json) -> Result<Vec<u8>, RenderError>`;
  - `typst::compile` followed by `typst_pdf::pdf` with
    `PdfStandard::A_3b` and a UTC timestamp;
  - convert Typst diagnostics into `{message, file, line, column, hint}`.
- [x] A dev CLI subcommand,
      `rustypdf render <template-dir> <data.json> -o out.pdf`, for manual
      testing and later benchmarks.
- [x] Unit tests:
  - `hello` renders;
  - a syntax error returns line and column;
  - a sandbox escape is rejected;
  - `/data.json` values appear in the output text.

**Done when**
- The CLI renders `templates/hello` with a JSON file into a valid PDF.
- That PDF passes veraPDF locally (`verapdf --flavour 3b out.pdf`).
- A broken template gives a readable error with the correct line and column.

---

## P2 — Bangla + English

**Goal:** bilingual text that is always correct, with each script in its own font.

**Tasks**
- [x] `templates/_shared/lang.typ`:
  - `setup` applies the per-script font list (Noto Sans Bengali via `covers`
    regex for U+0980–U+09FF and ZWNJ/ZWJ, then Inter) and sets `hyphenate: false`;
  - `num()` parses Bangla or ASCII digits into a number;
  - `digits-like()` prints a computed value in the same digit system as its input;
  - `bn()` marks a Bangla paragraph with `lang: "bn"`.
- [x] NFC-normalize every JSON string value before compile (`unicode-normalization`).
- [x] Add `templates/bilingual-test/`, a test template with a shaping corpus:
  - conjuncts: ক্ষ, জ্ঞ, ন্ত্র;
  - reph: র্ক;
  - split vowels: কি, কী, কো, কৌ;
  - ৎ and ঁ;
  - ৳ next to Bangla and ASCII digits;
  - mixed Bangla/English sentences, tables and headings.
- [x] Golden tests:
  - render page 1 with `typst-render` to PNG;
  - pixel-compare against `tests/golden/*.png`, with a small tolerance;
  - provide a `just golden-update` command to approve changes.
- [x] Tests that digits are never converted: ASCII stays ASCII and Bangla stays
      Bangla. `digits-like` follows its input.

**Done when**
- The golden images show correct Bangla shaping, checked by eye once by a
  Bangla reader, and the tests pass. *(Pending: the Bangla-reader review of
  `tests/golden/bilingual-test.png`.)*
- Text extracted from the PDF shows that Bangla glyphs use Noto Sans Bengali
  and Latin glyphs use Inter.
- The bilingual PDF still passes veraPDF 3b.

---

## P3 — HTTP API

**Goal:** the service is callable from other projects.

**Tasks**
- [x] `registry.rs`:
  - at startup, scan `TEMPLATES_DIR`; every folder with a `main.typ` is a
    template (folders starting with `_` are skipped);
  - store templates as `ArcSwap<HashMap<String, Arc<Template>>>`;
  - test-compile each template once at startup so errors appear immediately.
- [x] `POST /render/{template}`:
  - JSON body in, `application/pdf` out;
  - supports `?filename=` and `?inline=true`;
  - returns the `X-Render-Time-Ms` header.
- [x] Concurrency:
  - a `Semaphore` sized by `MAX_CONCURRENCY`, with compiles run in `spawn_blocking`;
  - `QUEUE_TIMEOUT_MS` returns a 503;
  - `RENDER_TIMEOUT_MS` sets a deadline per render.
- [x] Body size limit, returning a 413.
- [x] `error.rs`: one JSON error format for 404, 413, 422, 500 and 503 (PLAN.md §7).
- [x] `GET /templates`, `GET /templates/{t}`, `GET /healthz` and `GET /readyz`.
- [x] A periodic `comemo::evict(30)` task.
- [x] Graceful shutdown on SIGTERM that drains in-flight renders.
- [x] Integration tests with `axum::Router` + `tower::ServiceExt::oneshot`
      for every endpoint and every error code.

**Done when**
- `curl -X POST localhost:8080/render/hello -d '{...}' -o x.pdf` produces a valid PDF/A-3b.
- Every error path is covered by tests.
- 50 parallel requests complete with no errors.

---

## P4 — Static images + manifest + schema

**Goal:** templates can use logos and other files from their folder, and
declare their inputs.

**Tasks**
- [x] `images/assets.rs`:
  - preload every file under `<template>/assets/` and `_shared/assets/` into memory;
  - check the format by magic bytes (PNG, JPEG, GIF, WebP, SVG);
  - reject bad files at load time with a clear log message.
- [x] Serve those files through `World::file()`, so
      `#image("assets/logo.png")` and `#image("/_shared/assets/stamp.png")` work.
- [x] `manifest.toml` parsing (`serde` + `toml`):
  - `name`, `route`, `max_body_kb`, `timeout_ms`;
  - the `[images."<pointer>"]` tables, which are parsed now and used in P5.
- [x] `schema.json` validation (`jsonschema`), compiled once per template.
      Invalid input returns a 422 with `{path, msg}` for each error.
- [x] `sample.json` + `GET /preview/{template}`, which can be disabled by env.
- [x] Image fields in the request that hold a **file path** (e.g.
      `"assets/sign/manager.png"`) resolve to preloaded assets, inside the sandbox.
- [x] Tests:
  - a static logo renders;
  - a missing asset gives a clear error;
  - schema errors list the fields;
  - a path-traversal attempt in an image field is rejected.

**Done when**
- A template with a logo in `assets/` renders with the logo.
- Invalid input is rejected with a 422 before any compile work.

*Status: done. Covered by `tests/images.rs` with the `photo-test` fixture
(served at its manifest route `photos`); the real sector templates come in P7.*

---

## P5 — Dynamic images (URLs)

**Goal:** product photos and other per-request images, passed as URLs, are fast
and safe.

**Tasks**
- [x] `images/mod.rs`:
  - walk the manifest pointers (with `[]` meaning every array item) and decide
    each value's form: URL, `data:` URI, file path, or empty;
  - apply `optional`, `fallback` and `on_error`;
  - rewrite each value to `/__img/<sha256>.<ext>`.
- [x] `images/fetch.rs`:
  - `reqwest` with rustls (platform CA store; reqwest 0.13 has no `webpki-roots` feature), allowing only `http` and `https`;
  - a maximum of 3 redirects, checking each hop;
  - streaming size cap (`IMAGE_MAX_BYTES`) and timeout (`IMAGE_FETCH_TIMEOUT_MS`);
  - SSRF guard: resolve DNS and block private, loopback, link-local and metadata ranges;
  - optional `IMAGE_ALLOWED_HOSTS` (comma-separated, `*.` wildcard, case-insensitive).
- [x] Cache:
  - a `moka` LRU keyed by URL + `max_px`, with a byte budget (`IMAGE_CACHE_MB`);
  - a TTL from `Cache-Control`, defaulting to 1 h;
  - identical concurrent fetches merged into one.
- [x] Parallel resolution per request, limited by `IMAGE_FETCH_CONCURRENCY`
      and `IMAGE_MAX_PER_REQUEST`. This runs **before** the request takes a compile slot.
- [x] `images/resize.rs`: downscale to `max_px` with the `image` crate before caching.
- [x] `data:` URI decoding.
- [x] Return the `X-Render-Warnings` and `X-Fetch-Time-Ms` headers.
- [x] Tests, using a local mock HTTP server (`wiremock`):
  - success;
  - a 404 that uses the fallback;
  - a 404 with `on_error = "fail"` returning a 422;
  - a timeout;
  - an oversized image;
  - a disallowed host;
  - a private IP;
  - a redirect to a private IP;
  - a cache hit making no second request;
  - an invoice with 50 items fetching in parallel.

**Done when**
- An invoice with product photo URLs renders, and the second identical request
  makes zero network calls.
- A broken product link uses `no-photo.png` instead of failing the request.
- All SSRF and allowlist tests pass.

*Status: done, against the `photo-test` fixture and a `wiremock` server
(`tests/images.rs`). The retail invoice itself, with `no-photo.png`, comes in P7.*

---

## P6 — Docker + ops

**Goal:** a container that is safe to run in production.

**Tasks**
- [x] Multi-stage `Dockerfile`:
  - `cargo-chef` for dependency caching;
  - `distroless/cc` runtime image, running as nonroot;
  - fonts baked in.
- [x] `docker-compose.yml` with `templates` mounted read-only and the env from PLAN.md §10.
- [x] API key auth (`Authorization: Bearer`, keys from `API_KEYS`). It is
      skipped when `API_KEYS` is unset.
- [x] `GET /metrics` (Prometheus):
  - render latency per template;
  - image fetch latency and cache hit rate;
  - in-flight renders;
  - errors by type.
- [x] Structured JSON logs with a request ID, template name and timings.
- [x] Hot reload (`HOT_RELOAD`): watch with `notify`, debounce, rebuild the
      registry and assets, and swap atomically. A broken template keeps the
      old version and logs the error.
- [x] `POST /admin/reload`.
- [x] Check that the container runs with `--read-only` and that the image size is reasonable.
- [x] CI builds the Docker image.

**Done when**
- `docker compose up` runs the service and the curl examples from PLAN.md §13 work.
- Editing a template in the mounted folder takes effect without a restart.
- Requests without a valid key get a 401 when auth is on.

*Status: done. `tests/ops.rs` covers auth, metrics, request IDs, admin and
hot reload; the image was built and smoke-tested locally (`--read-only`,
nonroot, compose with a mounted folder) and CI builds and smoke-tests it.*

---

## P7 — Sector templates + CI quality gates

**Goal:** real templates that work out of the box, and CI that guarantees
output quality.

**Tasks**
- [x] Four bilingual invoice templates, each with `main.typ`, `manifest.toml`,
      `schema.json`, `sample.json` and `assets/logo.png`:
  - `invoice-retail`, with product photos from URLs;
  - `invoice-healthcare`;
  - `invoice-logistics`;
  - `invoice-construction`.
- [x] Shared `_shared/assets/no-photo.png` and `stamp.png`.
- [x] CI jobs:
  - render every template's `sample.json` (image URLs served by a mock server in CI);
  - **veraPDF 3b** on every output (veraPDF CLI from Maven Central via
    `scripts/verapdf.sh`, not the `verapdf/cli` Docker image), failing the build on any violation;
  - golden PNG tests for every template.
- [x] Add `docs/templates.md`, explaining how to write a new template:
  - folder layout;
  - `lang.typ`;
  - static vs dynamic images;
  - manifest and schema.

**Done when**
- All 4 templates render from their samples.
- All outputs pass veraPDF 3b in CI.
- Golden tests pass.

*Status: done. Sample photo URLs use the reserved host `cdn.rustypdf.test`,
served from `tests/fixtures/photos` (a `wiremock` server in the golden tests;
`python3 -m http.server` plus the dev-only `render --resolve` flag in
`just pdfa`). The retail sample has one deliberately broken photo link and one
item without a photo, to show the fallback. Pending: a Bangla reader's review
of the four new golden images.*

---

## P8 — Performance

**Goal:** meet the speed targets and prove it.

**Tasks**
- [x] `criterion` benchmarks of render alone (no HTTP), for each sector template.
- [x] Load tests with `oha` or `k6` against the Docker container, with images warm and cold.
- [x] Profile with `cargo flamegraph` and fix hot spots. Likely areas:
  - font loading;
  - JSON conversion;
  - image decoding;
  - comemo eviction settings.
- [x] Tune the defaults for `MAX_CONCURRENCY`, the eviction interval and the cache sizes.
- [x] Add `docs/performance.md`, recording results and the hardware used.

**Done when**
- These targets are met:
  - warm invoice of 1–2 pages: p50 < 15 ms and p99 < 50 ms on one core;
  - cold start under 1 s;
  - memory stays flat over a 30-minute load test.
- Benchmarks run in CI, which warns on a regression above 20%.

*Status: done, but the p50 < 15 ms target is only partly met: construction
(17 ms) and the HTTP path (16.4 ms) are over it. Details are in
`docs/performance.md`. Tools: `benches/render.rs` (criterion),
`examples/soak.rs` (in process), and `scripts/loadtest.sh` + `loadtest.js`
(k6 against the image). Profiled with `perf` directly (the same data
`cargo flamegraph` draws). The main finding: the old timer eviction let the
memo cache grow by about 1.7 MB per render; it is now evicted every
`MEMO_EVICT_EVERY` (16) renders. Also mimalloc, fat LTO, and `thumbnail`
resizing. New-data renders on one core: p50 13.4–14.6 ms for three
templates, 17.0 ms for construction, p99 under 40 ms. Same-data renders:
2.2–2.7 ms. Cold start: 83–314 ms. A 30-minute soak (198k requests) held
memory at 262–293 MiB. CI: `bench` job + `scripts/bench-ci.sh`.*

---

## P9 — Extras (optional)

- [ ] `POST /render/{template}/batch`, returning a ZIP or a merged PDF.
- [ ] An OpenAPI spec (`utoipa`) listing every template's route and schema.
- [ ] Client snippets for curl, TypeScript, Python and PHP in `docs/clients.md`.
- [ ] `GET /preview/{template}.png`, a PNG of page 1 for UI thumbnails.
- [ ] Bundled Typst `@preview` packages in `/packages`, if templates need them.

---

## Working rules for every phase

- One branch or PR per phase (or smaller). Every PR keeps CI green.
- New behavior comes with tests in the same PR.
- Never weaken a check to get green, whether a veraPDF failure, a golden
  diff or an SSRF test. Fix the cause, or update the golden images on purpose
  with `just golden-update` and a reason in the PR.
- Update `PLAN.md` when a decision changes.
