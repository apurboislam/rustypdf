# rustypdf

A small, fast HTTP service that renders [Typst](https://typst.app) templates
into **PDF/A-3b** documents from JSON. Bangla and English text each use their
own font (Noto Sans Bengali and Inter), even within one line.

- Design and decisions: [`PLAN.md`](PLAN.md)
- Build order and progress: [`BUILD.md`](BUILD.md)

## Status

Phases P0–P3 are done: render core, Bangla + English, and the HTTP API.
Images, manifests and schemas (P4–P5) and Docker (P6) come next.

## Quick start

```bash
just run                       # serves templates/ on :8080 (needs `just`; or: cargo run)

curl -X POST localhost:8080/render/hello \
  -H 'Content-Type: application/json' \
  -d '{"name": "রহিম", "message": "Hello from rustypdf"}' -o hello.pdf
```

Render without HTTP:

```bash
cargo run -- render templates/bilingual-test templates/bilingual-test/sample.json -o out.pdf
```

## Templates

Every folder in `TEMPLATES_DIR` that has a `main.typ` is served at
`POST /render/{folder}`. Folders starting with `_` (e.g. `_shared`) are not
templates. The request body is available to the template as JSON:

```typst
#import "/_shared/lang.typ": setup, num, digits-like
#let data = json("/data.json")
#show: setup
= Invoice #data.invoice_no
```

## API

| Method | Path | |
|---|---|---|
| `POST` | `/render/{template}` | JSON in, `application/pdf` out. `?filename=x.pdf`, `?inline=true`. Header `X-Render-Time-Ms`. |
| `GET` | `/templates`, `/templates/{template}` | Template list and details, with startup compile errors. |
| `GET` | `/healthz`, `/readyz` | Liveness and readiness. |

Errors are JSON: `{"error": "<kind>", "message": "...", "details": [...]}`.
They use these status codes:
- 404: unknown template;
- 413: body too large;
- 422: invalid JSON;
- 500: compile error, with `file`/`line`/`column`, or render timeout;
- 503: overloaded (retry).

## Configuration

These environment variables are read now; PLAN.md §9 lists them all.

| Var | Default |
|---|---|
| `PORT` | `8080` |
| `TEMPLATES_DIR` / `FONTS_DIR` | `/templates` / `/fonts` (falls back to `./templates` / `./fonts` when those don't exist) |
| `REQUIRED_FONTS` | `Noto Sans Bengali,Inter` (missing → refuse to start) |
| `MAX_CONCURRENCY` | CPU cores |
| `QUEUE_TIMEOUT_MS` | `5000` |
| `RENDER_TIMEOUT_MS` | `10000` |
| `MAX_BODY_KB` | `1024` |
| `LOG_FORMAT` | text; `json` for structured logs |

## Development

```bash
just lint            # cargo fmt --check + clippy -D warnings
just test            # all tests, including golden PNGs
just golden-update   # re-approve golden images (only on purpose)
just pdfa            # render every sample.json and check it with veraPDF 3b
```

`just pdfa` uses `verapdf` from `PATH` if installed. Otherwise it fetches the
veraPDF CLI from Maven Central, which needs `java` and `mvn`.

## License

Code: MIT (see `LICENSE`). Bundled fonts: SIL OFL 1.1 (see `fonts/`).
