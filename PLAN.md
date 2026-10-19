# rustypdf — Plan

A small, fast HTTP service that runs in Docker, loads Typst templates from a
mounted folder, exposes one render endpoint per template, and returns a PDF
built from the JSON variables you send.

```
your app ──POST /render/invoice-retail {json}──▶ rustypdf ──▶ application/pdf
```

---

## 1. Goals and non-goals

**Goals**
- Drop a template folder in, restart (or hot-reload), and you get an endpoint.
- Fast: a few ms to tens of ms per typical invoice, with no process spawn per request.
- Stateless and horizontally scalable. One small Docker image.
- Clear errors: bad input gives a 422 naming the field, and a broken template
  gives an error with the Typst line and column.

**Non-goals (v1)**
- No template editor UI, no storage of generated PDFs, no queue or async jobs.
- No HTML-to-PDF. Typst only.

---

## 2. Key technical decisions

| Decision | Choice | Why |
|---|---|---|
| Language | **Rust** | Typst is a Rust library, so we embed it directly instead of calling the `typst` CLI. That avoids process spawn, re-parsing and font re-scanning on every request. |
| Rendering | `typst` + `typst-pdf` crates (pinned version) | Compiles in-process and exports in-memory to `Vec<u8>`. |
| HTTP | `axum` + `tokio` | Fast, minimal, good ecosystem. |
| CPU work | Bounded blocking pool (`spawn_blocking` + `Semaphore`) | Typst compile is CPU-bound and must not block the async runtime. |
| Fonts | Loaded **once** at startup into a shared `FontBook` (embedded defaults via `typst-assets` + `/fonts` dir) | Font scanning is the most expensive cold-start step. |
| Variable injection | A virtual file `/data.json` served by our `World` | Templates read `#let data = json("/data.json")`. Works with nested objects and arrays such as line items, and needs no string escaping. |
| Validation | Optional `schema.json` per template (`jsonschema` crate) | Bad input fails fast with a 422 before compile. |
| Config | Env vars + optional `manifest.toml` per template | Easy to set in Docker. |

### Why it will be fast
1. **No subprocess.** Typst runs as a library inside the server.
2. **Warm state.** Fonts, template sources, images and package files are parsed or loaded once and shared (`Arc`) across requests.
3. **Typst's incremental engine (`comemo`).** Repeated compiles of the same
   template with different data reuse memoized layout work. We call
   `comemo::evict(n)` periodically so memory stays bounded.
4. **No disk I/O per request.** Everything is in memory, and `/data.json` is
   synthesized from the request body.
5. **Parallel.** N concurrent renders, with N = CPU cores by default.

Target numbers to check in benchmarks: an invoice of 1–2 pages at p50 < 15 ms
and p99 < 50 ms warm on a single core, and cold start < 500 ms.

---

## 3. Template layout (what you provide)

Mount a folder into the container at `/templates`. Each subfolder is one template
and one endpoint:

```
templates/
├── invoice-retail/
│   ├── main.typ          # required: entry point
│   ├── schema.json       # optional: JSON Schema for the input
│   ├── manifest.toml     # optional: metadata / overrides
│   ├── sample.json       # optional: used by /preview and tests
│   └── assets/logo.png   # optional: images, sub-modules, etc.
├── invoice-healthcare/
├── invoice-logistics/
└── invoice-construction/
```

Example `main.typ`:

```typst
#let data = json("/data.json")

#set page(paper: "a4", margin: 2cm)
#set text(font: "Inter", size: 10pt)

#image("assets/logo.png", width: 4cm)
= Invoice #data.invoice_no
*Billed to:* #data.customer.name \
#data.customer.address

#table(
  columns: (1fr, auto, auto, auto),
  [*Item*], [*Qty*], [*Price*], [*Total*],
  ..data.items.map(i => (i.name, str(i.qty), str(i.price), str(i.qty * i.price))).flatten()
)

*Grand total:* #data.total
```

Optional `manifest.toml`:

```toml
name = "Retail invoice"
route = "invoice-retail"   # defaults to folder name
max_body_kb = 512
timeout_ms = 5000
```

Shared bits such as a company header or styles can sit in `templates/_shared/`.
Templates import them with `#import "/_shared/brand.typ": *`, because the
template root is `/templates` (see section 6).

---

## 4. HTTP API

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/render/{template}` | Body = JSON variables. Returns `application/pdf`. |
| `GET`  | `/templates` | Lists templates, with schema and sample if present. |
| `GET`  | `/templates/{template}` | Metadata + JSON schema for one template. |
| `GET`  | `/preview/{template}` | Renders with `sample.json` (dev aid, can be disabled). |
| `POST` | `/admin/reload` | Re-scans `/templates` (also automatic via file watcher). |
| `GET`  | `/healthz`, `/readyz` | Liveness and readiness. |
| `GET`  | `/metrics` | Prometheus: render latency histogram per template, errors, in-flight. |

**Request**
```http
POST /render/invoice-retail
Authorization: Bearer <API_KEY>
Content-Type: application/json

{ "invoice_no": "R-1001", "customer": {"name": "ACME"}, "items": [...], "total": "120.00" }
```

**Query options:** `?filename=R-1001.pdf` sets `Content-Disposition`, and
`?inline=true` returns the PDF inline instead of as an attachment.

**Responses**
- `200 application/pdf`, with header `X-Render-Time-Ms` added.
- `404` unknown template.
- `413` body too large.
- `422` schema validation failed: `{"error":"validation","details":[{"path":"/items/0/qty","msg":"expected number"}]}`
- `500` Typst compile error: `{"error":"compile","details":[{"message":"unknown variable: totl","file":"main.typ","line":24,"column":3,"hint":"..."}]}`
- `503` overloaded, when the render queue is full. The caller can retry.

**Later (v1.1):** `POST /render/{template}/batch` takes an array of payloads and
returns a ZIP or a merged PDF.

---

## 5. Architecture

```
            ┌──────────────────── rustypdf (single binary) ────────────────────┐
HTTP ──▶ axum router ──▶ auth / size limit ──▶ handler
                                                   │
                                     TemplateRegistry (ArcSwap<HashMap>)
                                     ├─ name → Template { root, main.typ Source,
                                     │          files cache, schema validator }
                                     │
                                     ▼  Semaphore (N = cores) + spawn_blocking
                               RenderWorld (per request, cheap)
                               ├─ shared: Library, FontBook, Fonts  (Arc, startup)
                               ├─ shared: template sources/assets   (Arc, per template)
                               └─ per-request: "/data.json" bytes
                                     │
                          typst::compile ─▶ typst_pdf::pdf ─▶ Vec<u8>
            └───────────────────────────────────────────────────────────────────┘
                       file watcher (notify) ──▶ rebuild registry ──▶ swap
```

### Modules (crate layout)
```
src/
├── main.rs          # config, tracing, startup, graceful shutdown
├── config.rs        # env: PORT, TEMPLATES_DIR, FONTS_DIR, API_KEYS, MAX_CONCURRENCY, ...
├── fonts.rs         # load embedded + system + /fonts once → Arc<FontStore>
├── registry.rs      # scan templates dir, parse manifest/schema, hot reload
├── world.rs         # impl typst::World for RenderWorld
├── render.rs        # compile + pdf export, error → diagnostics mapping
├── api/
│   ├── mod.rs       # router
│   ├── render.rs    # POST /render/{t}
│   ├── templates.rs # list/info/preview
│   └── ops.rs       # health, metrics, reload
└── error.rs         # AppError → HTTP response
```

### The `World` implementation (core piece)
- `library()`: a `LazyHash<Library>` built once at startup. The standard library has no per-request state because data comes from a file, not `sys.inputs`.
- `book()` / `font(i)`: shared `FontStore`. Font bytes are lazily parsed and cached in `OnceLock`.
- `main()`: the template's `main.typ` FileId.
- `source(id)` / `file(id)`:
  - `/data.json` returns the request body bytes.
  - Other paths resolve from the template root's in-memory cache. Files are
    loaded on first access and invalidated by the watcher.
  - Paths that escape the template root are rejected, so templates are sandboxed.
- `today()`: the current date, so `datetime.today()` works in templates.
- **Typst packages** (`@preview/...`): resolved from a local package dir
  (`/packages`) pre-populated at image build time. There is **no network at
  render time**, which keeps rendering fast and deterministic.

### Concurrency and limits
- `MAX_CONCURRENCY` (default = cores) caps simultaneous compiles.
- `QUEUE_TIMEOUT_MS` sets how long a request waits for a slot before it gets a 503.
- `RENDER_TIMEOUT_MS` is a per-request deadline, which catches runaway templates
  such as infinite loops.
- A `comemo::evict(30)` every N renders or on a timer bounds the memo cache.

---

## 6. Docker

Multi-stage build with `cargo-chef` for dependency caching, and a small runtime image.

```dockerfile
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json .
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12
COPY --from=builder /app/target/release/rustypdf /rustypdf
COPY fonts /fonts
ENV TEMPLATES_DIR=/templates FONTS_DIR=/fonts PORT=8080
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/rustypdf"]
```

`docker-compose.yml` for use alongside your other projects:

```yaml
services:
  rustypdf:
    image: rustypdf:latest
    ports: ["8080:8080"]
    volumes:
      - ./templates:/templates:ro
      - ./fonts:/fonts:ro          # optional: brand fonts
    environment:
      API_KEYS: "change-me"
      MAX_CONCURRENCY: "4"
      HOT_RELOAD: "true"
```

Two deployment modes:
- **Mounted templates** (dev and most setups) use the compose file above, with hot reload.
- **Baked templates** (immutable prod) use `FROM rustypdf` and `COPY templates /templates`, with hot reload off.

---

## 7. Security
- API key auth via `Authorization: Bearer` (comma-separated `API_KEYS`), optional so it can be off on a private network.
- Body size limit, render timeout and concurrency cap.
- Filesystem sandbox: templates can only read inside `/templates`, `/fonts` and `/packages`.
- No network access from templates.
- Runs as a non-root user with a read-only root filesystem.

---

## 8. Milestones

1. **M1 – Core render (MVP)**
   - Cargo project, `World` impl, font loading, compile one template from disk to a PDF in a unit test.
   - `POST /render/{template}`, registry scan at startup, error mapping.
2. **M2 – Production basics**
   - JSON Schema validation, `manifest.toml`, auth, limits, timeouts.
   - `/healthz`, `/readyz`, `/metrics`, structured logs (`tracing`).
   - Dockerfile + compose, plus 4 example invoice templates (retail, healthcare, logistics, construction) with `sample.json`.
3. **M3 – Speed and DX**
   - Benchmarks (`criterion` + `oha`/`k6` load test) and tuning of eviction and concurrency.
   - Hot reload (`notify`), `/preview`, `/templates` listing, offline Typst packages.
4. **M4 – Extras (optional)**
   - Batch endpoint (ZIP or merged PDF), PDF/A output option, PNG preview of page 1.
   - Client snippets (curl, TS, Python, PHP) and an OpenAPI spec (`utoipa`).

**Testing:** unit tests for the world and file resolution, golden tests that
render each `sample.json` and check page count and extracted text, and API
integration tests with `axum::Router` + `tower::ServiceExt`.

---

## 9. Calling it from another project

```bash
curl -X POST http://localhost:8080/render/invoice-healthcare \
  -H "Authorization: Bearer change-me" \
  -H "Content-Type: application/json" \
  -d @invoice.json -o invoice.pdf
```

```ts
const res = await fetch(`${PDF_URL}/render/invoice-logistics`, {
  method: "POST",
  headers: { "Content-Type": "application/json", Authorization: `Bearer ${KEY}` },
  body: JSON.stringify(invoice),
});
const pdf = Buffer.from(await res.arrayBuffer());
```

---

## 10. Open questions
1. Should endpoints be **generic** (`/render/{template}`) or also literal
   per-template routes in the OpenAPI spec? The plan does both: one handler, with
   routes listed per template in `/templates` and the OpenAPI spec.
2. Are Typst `@preview` packages needed? If so, which ones should be pre-bundled in the image?
3. Is PDF/A required, for example for e-invoicing archives?
4. Will a single instance be enough, or will several replicas run behind a load balancer? The design works either way since it is stateless.
