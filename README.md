# rustypdf

A small, fast HTTP service that renders [Typst](https://typst.app) templates
into **PDF/A-3b** documents from JSON. Bangla and English text each use their
own font (Noto Sans Bengali and Inter), even within one line.

- Design and decisions: [`PLAN.md`](PLAN.md)
- Build order and progress: [`BUILD.md`](BUILD.md)

## Status

Phases P0–P7 are done: render core, Bangla + English, HTTP API, static and
URL images with manifests and schemas, Docker + ops (auth, metrics, hot
reload), and four bilingual sector invoices (`invoice-retail`,
`invoice-healthcare`, `invoice-logistics`, `invoice-construction`).
Performance work (P8) comes next. To write your own template, see
[`docs/templates.md`](docs/templates.md).

## Quick start

```bash
just run                       # serves templates/ on :8080 (needs `just`; or: cargo run)

curl -X POST localhost:8080/render/hello \
  -H 'Content-Type: application/json' \
  -d '{"name": "রহিম", "message": "Hello from rustypdf"}' -o hello.pdf
```

With Docker:

```bash
just docker                    # builds rustypdf:latest
docker compose up              # templates/ mounted read-only, hot reload, API key "change-me"
curl -X POST localhost:8080/render/hello -H 'Authorization: Bearer change-me' \
  -H 'Content-Type: application/json' -d '{"name": "রহিম"}' -o hello.pdf
```

Render without HTTP:

```bash
cargo run -- render templates/bilingual-test templates/bilingual-test/sample.json -o out.pdf
```

## Templates

Every folder in `TEMPLATES_DIR` that has a `main.typ` is served at
`POST /render/{folder}` (or the `route` from its `manifest.toml`). Folders
starting with `_` (e.g. `_shared`) are not templates. Optional files:
`schema.json` (JSON Schema for the body), `manifest.toml` (name, route,
limits, image fields), `sample.json` (used by `/preview` and the startup
check) and `assets/` (static images). The request body is available to the
template as JSON:

```typst
#import "/_shared/lang.typ": setup, num, digits-like
#let data = json("/data.json")
#show: setup
= Invoice #data.invoice_no
```

## API

| Method | Path | |
|---|---|---|
| `POST` | `/render/{template}` | JSON in, `application/pdf` out. `?filename=x.pdf`, `?inline=true`. Headers `X-Render-Time-Ms`, `X-Fetch-Time-Ms`, `X-Render-Warnings` (image fallbacks), `X-Request-Id`. |
| `GET` | `/templates`, `/templates/{template}` | Template list and details: schema, image fields, sample, startup errors. |
| `GET` | `/preview/{template}` | Renders `sample.json` inline (`PREVIEW=false` disables). |
| `POST` | `/admin/reload` | Re-scans the templates folder now. |
| `GET` | `/metrics` | Prometheus metrics. |
| `GET` | `/healthz`, `/readyz` | Liveness and readiness (never need an API key). |

Errors are JSON: `{"error": "<kind>", "message": "...", "details": [...]}`.
They use these status codes:
- 401: missing or wrong API key (when `API_KEYS` is set);
- 404: unknown template;
- 413: body too large;
- 422: invalid JSON, schema violation (`validation`) or image error (`image`);
- 500: compile error, with `file`/`line`/`column`, or render timeout;
- 503: overloaded (retry).

## Configuration

PLAN.md §9 describes each one.

| Var | Default |
|---|---|
| `API_KEYS` | unset = auth off; comma-separated bearer tokens |
| `PORT` | `8080` |
| `TEMPLATES_DIR` / `FONTS_DIR` | `/templates` / `/fonts` (falls back to `./templates` / `./fonts` when those don't exist) |
| `REQUIRED_FONTS` | `Noto Sans Bengali,Inter` (missing → refuse to start) |
| `MAX_CONCURRENCY` | CPU cores |
| `QUEUE_TIMEOUT_MS` | `5000` |
| `RENDER_TIMEOUT_MS` | `10000` |
| `MAX_BODY_KB` | `1024` |
| `LOG_FORMAT` | text; `json` for structured logs (the Docker image sets `json`) |
| `IMAGE_FETCH_TIMEOUT_MS` / `IMAGE_MAX_BYTES` | `3000` / `5242880` |
| `IMAGE_MAX_PER_REQUEST` / `IMAGE_FETCH_CONCURRENCY` | `200` / `16` |
| `IMAGE_CACHE_MB` | `256` |
| `IMAGE_ALLOWED_HOSTS` | unset = any public host; `*.` wildcard |
| `HOT_RELOAD` / `PREVIEW` | `true` / `true` |

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
