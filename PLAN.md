# rustypdf — Plan

A small, fast HTTP service that runs in Docker, loads Typst templates from a
mounted folder, exposes one render endpoint per template, and returns a
**PDF/A-3b** document built from the JSON variables you send. It supports
**Bangla and English** text, each in its own font, and fetches **images passed
as public URLs**.

```
your app ──POST /render/invoice-retail {json + image URLs}──▶ rustypdf ──▶ application/pdf (PDF/A-3b)
```

---

## 1. Goals and non-goals

**Goals**
- Drop a template folder in, restart (or hot-reload), and you get an endpoint.
- Every output is **PDF/A-3b** compliant, which we verify with veraPDF in CI.
- Bangla (বাংলা) and English shape correctly, including Bangla conjuncts
  (যুক্তাক্ষর), vowel signs (কার) and reph. Each script uses its own font, even
  inside one sentence.
- Image variables are given as public URLs. The service fetches them, caches
  them and embeds them.
- Fast: a few ms to tens of ms per typical invoice when images are cached, with
  no process spawn per request.
- Stateless and horizontally scalable. One small Docker image.
- Clear errors:
  - bad input gives a 422 naming the field;
  - an image that fails to load gives a 422 naming the field and URL;
  - a broken template gives an error with the Typst line and column.

**Non-goals (v1)**
- No template editor UI, no storage of generated PDFs, no queue or async jobs.
- No HTML-to-PDF. Typst only.

---

## 2. Key technical decisions

| Decision | Choice | Why |
|---|---|---|
| Language | **Rust** | Typst is a Rust library, so we embed it directly instead of calling the `typst` CLI. That avoids process spawn, re-parsing and font re-scanning per request. |
| Rendering | `typst` + `typst-pdf` crates (pinned version) | Compiles in-process and exports in-memory to `Vec<u8>`. |
| PDF standard | `typst_pdf::PdfOptions` with `PdfStandard::A_3b` | Typst can export PDF/A-3b natively, so no post-processing step is needed. See section 5. |
| Text shaping | Typst's built-in shaper (rustybuzz, a HarfBuzz port) | Handles the Bengali shaping rules (conjuncts, reph, pre-base vowel reordering). |
| Bangla/English fonts | Per-script font lists using Typst's `covers` (Unicode range regex) | Bangla characters always use the Bangla font and Latin characters always use the English font. See section 4. |
| HTTP | `axum` + `tokio` | Fast, minimal, good ecosystem. |
| Image fetching | `reqwest` (rustls) + `moka` LRU cache | Fetches in parallel with timeouts, and repeated images such as logos and signatures load from memory. |
| CPU work | Bounded blocking pool (`spawn_blocking` + `Semaphore`) | Typst compile is CPU-bound and must not block the async runtime. |
| Fonts | Baked into the image and loaded **once** at startup into a shared `FontBook` | Font scanning is the most expensive cold-start step. |
| Variable injection | A virtual file `/data.json` served by our `World` | Templates read `#let data = json("/data.json")`. Works with nested objects and arrays such as line items. |
| Validation | Optional `schema.json` per template (`jsonschema` crate) | Bad input fails fast with a 422 before any fetch or compile. |

### Why it will be fast
1. **No subprocess.** Typst runs as a library inside the server.
2. **Warm state.** Fonts, template sources and static assets are parsed once
   and shared (`Arc`) across requests.
3. **Typst's incremental engine (`comemo`).** Repeated compiles of the same
   template with different data reuse memoized work. A periodic
   `comemo::evict(n)` keeps memory bounded.
4. **Images cached in memory.** A URL is fetched once and then served from the
   LRU cache. Fetches within one request run in parallel.
5. **Parallel renders.** N concurrent renders, with N = CPU cores by default.

Target numbers to check in benchmarks:
- 1–2 page invoice, warm and images cached: p50 < 15 ms, p99 < 50 ms on one core.
- First use of a new image URL adds the network round-trip, capped by `IMAGE_FETCH_TIMEOUT_MS`.
- Cold start: < 1 s.

---

## 3. Template layout (what you provide)

Mount a folder into the container at `/templates`. Each subfolder is one
template and one endpoint:

```
templates/
├── _shared/
│   └── lang.typ          # shared font setup, digit-aware number helpers
├── invoice-retail/
│   ├── main.typ          # required: entry point
│   ├── schema.json       # optional: JSON Schema for the input
│   ├── manifest.toml     # optional: metadata, image fields, overrides
│   ├── sample.json       # optional: used by /preview and tests
│   └── assets/logo.png   # optional: static images, sub-modules, etc.
├── invoice-healthcare/
├── invoice-logistics/
└── invoice-construction/
```

The template root is `/templates`, so templates can
`#import "/_shared/lang.typ": *`.

Example `main.typ`:

```typst
#import "/_shared/lang.typ": setup, num, digits-like
#let data = json("/data.json")

#set document(title: "Invoice " + data.invoice_no)   // PDF/A metadata
#show: setup                                         // Bangla + English fonts

#set page(paper: "a4", margin: 2cm)

#image(data.company_logo, width: 4cm)                // image from URL variable
= চালান / Invoice #data.invoice_no
*গ্রাহক / Billed to:* #data.customer.name \
#data.customer.address

#table(
  columns: (1fr, auto, auto, auto),
  [*পণ্য / Item*], [*পরিমাণ / Qty*], [*দর / Price*], [*মোট / Total*],
  ..data.items.map(i => (
    i.name, str(i.qty), str(i.price),
    // computed value uses the same digits as its input (৪৫০ → ৯০০, 450 → 900)
    digits-like(num(i.qty) * num(i.price), i.price),
  )).flatten()
)

*সর্বমোট / Grand total:* ৳ #data.total            // printed exactly as sent

#if "signature" in data [ #image(data.signature, height: 1.5cm) ]
```

`manifest.toml`:

```toml
name = "Retail invoice"
route = "invoice-retail"          # defaults to folder name
max_body_kb = 512
timeout_ms = 5000

# Fields in the payload that hold image URLs (JSON pointers; [] = every array item)
images = ["/company_logo", "/signature", "/items/[]/photo"]
on_image_error = "fail"           # "fail" (422) | "placeholder" (transparent 1×1 image)
```

---

## 4. Bangla + English support

### Fonts
These are baked into the image under `/fonts` and loaded at startup. Each
template, or the shared `lang.typ`, chooses which to use.

| Script | Font (bundled) | Notes |
|---|---|---|
| Bangla | **Noto Sans Bengali** (OFL), all weights | The default and required Bangla font |
| English | **Inter** (OFL), all weights | Can be swapped for a brand font in `/fonts` |

Any extra `.ttf`/`.otf` files mounted into `/fonts` are picked up as well, for
example a brand font. At startup the service checks that every font named in
`REQUIRED_FONTS` exists, and refuses to start otherwise. This prevents
documents from silently rendering in a fallback font. Typst always embeds the
fonts it uses, as PDF/A requires.

### Per-script font selection
Typst's font list accepts a `covers` regex, which ties a font to a Unicode
range. Bangla characters (U+0980–U+09FF, including ৳ and ০–৯) always use the
Bangla font. Everything else uses the English font, even when the two scripts
are mixed in one line:

```typst
// templates/_shared/lang.typ
#let bn-font = "Noto Sans Bengali"
#let en-font = "Inter"

#let setup(body) = {
  set text(
    font: (
      (name: bn-font, covers: regex("[\u{0980}-\u{09FF}\u{200C}\u{200D}]")),
      en-font,
    ),
    size: 10pt,
    hyphenate: false,       // no reliable Bangla hyphenation patterns; avoid broken words
  )
  body
}

#let bn-digits = "০১২৩৪৫৬৭৮৯".clusters()

// "১২৩৪.৫০" or "1234.50" or 1234.5 → number, for arithmetic
#let num(v) = if type(v) == str {
  float(v.clusters().map(c => {
    let i = bn-digits.position(d => d == c)
    if i != none { str(i) } else { c }
  }).join().replace(",", ""))
} else { v }

// Format n with Bangla digits only if `ref` (the input it came from) used them
#let digits-like(n, ref) = {
  let s = str(n)
  if type(ref) == str and ref.clusters().any(c => c in bn-digits) {
    s.clusters().map(c => if c >= "0" and c <= "9" { bn-digits.at(int(c)) } else { c }).join()
  } else { s }
}

// Bangla-only paragraph (correct line-breaking language)
#let bn(body) = text(lang: "bn", body)
```

The font list also handles the case where the English font has no Bangla
glyphs, and the reverse. Templates can still override fonts per element.

### Digits rule
**There is no automatic digit conversion.** Digits print exactly as they
arrive:
- Bangla digits (০–৯) in the request body or written in the template print as
  Bangla digits, in Noto Sans Bengali.
- ASCII digits (0–9) print as ASCII digits, in Inter.

Typst cannot do arithmetic on Bangla-digit strings, so `lang.typ` provides two
helpers for templates that compute values (line totals, VAT):
- `num()` parses either digit system into a number.
- `digits-like(value, input)` prints the result with Bangla digits only if the
  input it came from used them.

Callers that want ASCII digits should send ASCII digits, and callers that want
Bangla digits should send Bangla digits. The service does not decide for them.

### Correctness checks
- **Golden image tests:** each template's Bangla sample is rendered to PNG with
  `typst-render` and pixel-compared against approved reference images. This
  catches shaping or font regressions, such as broken conjuncts or a wrong
  fallback font, when Typst or the fonts are upgraded.
- **Test corpus:** words that stress the shaper, such as ক্ষ, জ্ঞ, ন্ত্র, র্ক
  (reph), কি/কী/কো/কৌ (split vowels), ৎ and ঁ, mixed with English in one line.
  The corpus also covers Bangla and ASCII digits side by side, to check that
  digits are never converted.
- **Input normalization:** text is NFC-normalized before compile (with the
  `unicode-normalization` crate), so data from different keyboards and input
  methods shapes the same way.

---

## 5. PDF/A-3b

- Export uses `PdfOptions { standards: [PdfStandard::A_3b], timestamp: <now, UTC>, ident: <deterministic> }`.
- Typst enforces the PDF/A rules itself: fonts embedded, an sRGB output intent,
  and XMP metadata. A template that breaks the standard fails at compile time
  with a clear error rather than producing a non-compliant PDF.
- Metadata: the document title comes from `#set document(...)` in the template.
  Otherwise the default is `manifest.name`. Author and keywords can come from
  the manifest.
- Why **3b** suits Bangla: level "b" (basic) only requires that the PDF
  displays correctly. Level "u" also requires every glyph to map to Unicode,
  and Bangla conjunct glyphs often can't map cleanly. So 3b is the right
  choice, and Bangla text still remains largely searchable and copyable.
- **No embedded files:** the output is the PDF alone, with no XML or other
  attachments.
- **Verification:** CI runs **veraPDF** (`--flavour 3b`) on the output of every
  template's `sample.json`. The build fails if any of them is non-compliant.
- Images: PNG/JPEG/GIF/SVG/WebP are re-encoded by Typst in a form PDF/A
  allows, and transparency is permitted in PDF/A-2 and later. CMYK and ICC
  edge cases are covered by tests.

---

## 6. Image variables (public URLs)

### Flow per request
1. Validate the JSON against `schema.json` (if present).
2. Collect every URL at the manifest's `images` pointers.
3. Fetch them **in parallel**, using the cache first.
4. Store each image's bytes as a virtual file
   `/__img/<sha256-of-url>.<ext>` in the request's `World`.
5. Rewrite the JSON value from the URL to that virtual path.
6. Compile. The template just calls `#image(data.company_logo)`.

The template never sees the network. It only reads the rewritten path.

### Fetcher rules
- Only `http`/`https`, with a maximum of 3 redirects, checking every hop.
- Limits:
  - `IMAGE_MAX_BYTES` (default 5 MB), enforced while streaming;
  - `IMAGE_FETCH_TIMEOUT_MS` (default 3000);
  - `IMAGE_MAX_PER_REQUEST` (default 20).
- Format is checked by magic bytes (PNG, JPEG, GIF, WebP, SVG), not by the
  `Content-Type` header.
- **SSRF protection:** the hostname is resolved, and private, loopback,
  link-local and cloud-metadata addresses (10/8, 172.16/12, 192.168/16,
  127/8, 169.254/16, ::1, fc00::/7) are rejected. This always applies.
- **Optional host allowlist:** the `IMAGE_ALLOWED_HOSTS` env variable takes a
  comma-separated list, for example
  `IMAGE_ALLOWED_HOSTS=cdn.example.com,*.s3.amazonaws.com`.
  - A `*.` prefix matches any subdomain.
  - Whitespace is trimmed and matching is case-insensitive.
  - The check applies to every redirect hop.
  - When the variable is unset or empty, any public host is allowed. A
    disallowed host returns a 422 naming the field and URL.
- **Cache:** an in-memory `moka` LRU keyed by URL, with a byte budget
  (`IMAGE_CACHE_MB`, default 256) and a TTL that follows the image's
  `Cache-Control` header (default 1 h). Identical concurrent fetches are
  merged into one.
- **On failure:** `on_image_error = "fail"` returns
  `422 {"error":"image","details":[{"path":"/signature","url":"...","msg":"timeout"}]}`.
  `"placeholder"` substitutes a transparent image and adds an
  `X-Render-Warnings` header.
- **Also accepted:** `data:image/png;base64,...` URIs, for callers that want to
  avoid the round-trip.

---

## 7. HTTP API

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/render/{template}` | Body = JSON variables (including image URLs). Returns `application/pdf` (PDF/A-3b). |
| `GET`  | `/templates` | Lists templates, with schema, image fields and sample. |
| `GET`  | `/templates/{template}` | Metadata + JSON schema for one template. |
| `GET`  | `/preview/{template}` | Renders with `sample.json` (dev aid, can be disabled). |
| `POST` | `/admin/reload` | Re-scans `/templates` (also automatic via file watcher). |
| `GET`  | `/healthz`, `/readyz` | Liveness and readiness (fonts loaded, templates compiled). |
| `GET`  | `/metrics` | Prometheus metrics. See below. |

`/metrics` exposes:
- render latency per template;
- image fetch latency and cache hit rate;
- errors;
- in-flight renders.

**Request**
```http
POST /render/invoice-retail
Authorization: Bearer <API_KEY>
Content-Type: application/json

{
  "invoice_no": "R-1001",
  "company_logo": "https://cdn.example.com/brand/logo.png",
  "customer": { "name": "রহিম এন্টারপ্রাইজ", "address": "মিরপুর, ঢাকা" },
  "items": [ { "name": "চাল (Rice) 5kg", "qty": 2, "price": 450, "photo": "https://cdn.example.com/p/rice.jpg" } ],
  "total": "900.00",
  "signature": "https://cdn.example.com/sign/manager.png"
}
```

**Query options:** `?filename=R-1001.pdf` sets `Content-Disposition`, and
`?inline=true` returns the PDF inline.

**Responses**
- `200 application/pdf`, with headers `X-Render-Time-Ms` and `X-Fetch-Time-Ms`.
- `404` unknown template.
- `413` body too large.
- `422` validation or image error, naming the field.
- `500` Typst compile error, including PDF/A violations, with file, line and column.
- `503` overloaded, when the render queue is full. The caller can retry.

**Later:** `POST /render/{template}/batch` returns a ZIP or a merged PDF.

---

## 8. Architecture

```
            ┌──────────────────────── rustypdf (single binary) ────────────────────────┐
HTTP ──▶ axum ──▶ auth / size limit ──▶ handler
                                          │ 1. schema validate
                                          │ 2. NFC normalize strings
                                          │ 3. ImageFetcher ──▶ moka cache ──miss──▶ reqwest (SSRF-guarded)
                                          │      (parallel)      ▲ shared across requests
                                          ▼ 4. rewrite URLs → /__img/<hash>
                          TemplateRegistry (ArcSwap<HashMap>)
                                          │
                       Semaphore (N = cores) + spawn_blocking
                                          ▼
                               RenderWorld (per request, cheap)
                               ├─ shared: Library, FontBook, Fonts (bn + en)  (startup)
                               ├─ shared: template sources/assets             (per template)
                               └─ per-request: /data.json, /__img/* bytes
                                          │
                 typst::compile ─▶ typst_pdf::pdf(PdfStandard::A_3b) ─▶ Vec<u8>
            └───────────────────────────────────────────────────────────────────────────┘
               file watcher (notify) ──▶ rebuild registry ──▶ atomic swap
```

### Modules (crate layout)
```
src/
├── main.rs          # config, tracing, startup, graceful shutdown
├── config.rs        # env vars (see §9)
├── fonts.rs         # load /fonts once, verify REQUIRED_FONTS → Arc<FontStore>
├── registry.rs      # scan templates dir, parse manifest/schema, hot reload
├── images.rs        # URL extraction by pointer, fetch, SSRF guard, cache, rewrite
├── world.rs         # impl typst::World for RenderWorld (sandboxed file access)
├── render.rs        # compile + PDF/A-3b export, diagnostics mapping
├── api/
│   ├── mod.rs       # router
│   ├── render.rs    # POST /render/{t}
│   ├── templates.rs # list/info/preview
│   └── ops.rs       # health, metrics, reload
└── error.rs         # AppError → HTTP response
```

### The `World` implementation (core piece)
- `library()`: built once at startup.
- `book()` / `font(i)`: shared `FontStore`. Font bytes are lazily parsed and cached.
- `main()`: the template's `main.typ`.
- `source(id)` / `file(id)`:
  - `/data.json` returns the rewritten request JSON.
  - `/__img/*` returns the fetched image bytes for this request.
  - Other paths resolve from the template root's in-memory cache.
  - Paths that escape the template root are rejected.
- `today()`: the current date.
- **Typst packages** (`@preview/...`): resolved from `/packages`, which is
  pre-populated at image build time. Templates have no network access.

### Concurrency and limits
- `MAX_CONCURRENCY` caps simultaneous compiles. `QUEUE_TIMEOUT_MS` returns a
  503 when no slot frees up in time. `RENDER_TIMEOUT_MS` sets a deadline.
- Image fetches happen **before** the request takes a compile slot, so slow
  networks never block CPU workers.
- A periodic `comemo::evict(30)` bounds the memo cache.

---

## 9. Configuration (env)

| Var | Default | Meaning |
|---|---|---|
| `PORT` | 8080 | HTTP port |
| `TEMPLATES_DIR` / `FONTS_DIR` | `/templates` / `/fonts` | Mount points |
| `API_KEYS` | (none = auth off) | Comma-separated bearer tokens |
| `REQUIRED_FONTS` | `Noto Sans Bengali,Inter` | Refuse to start if any is missing |
| `MAX_CONCURRENCY` | CPU cores | Parallel compiles |
| `RENDER_TIMEOUT_MS` | 10000 | Per-render deadline |
| `IMAGE_FETCH_TIMEOUT_MS` | 3000 | Per-image fetch timeout |
| `IMAGE_MAX_BYTES` | 5242880 | Per-image size cap |
| `IMAGE_MAX_PER_REQUEST` | 20 | Image count cap |
| `IMAGE_CACHE_MB` | 256 | Image cache budget |
| `IMAGE_ALLOWED_HOSTS` | (unset = any public host) | Optional comma-separated allowlist, `*.` wildcard for subdomains |
| `HOT_RELOAD` | true | Watch templates dir |

---

## 10. Docker

The image uses a multi-stage build with `cargo-chef` and a small runtime base.
Bangla and English fonts are **baked in**, so output is identical everywhere.

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
COPY fonts /fonts                 # Noto Sans Bengali + Inter, all weights (OFL)
ENV TEMPLATES_DIR=/templates FONTS_DIR=/fonts PORT=8080
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/rustypdf"]
```

`docker-compose.yml`:

```yaml
services:
  rustypdf:
    image: rustypdf:latest
    ports: ["8080:8080"]
    volumes:
      - ./templates:/templates:ro
      - ./brand-fonts:/fonts/extra:ro   # optional extra fonts
    environment:
      API_KEYS: "change-me"
      MAX_CONCURRENCY: "4"
      IMAGE_ALLOWED_HOSTS: "cdn.example.com,*.s3.amazonaws.com"   # optional; omit to allow any public host
      HOT_RELOAD: "true"
```

The container needs **outbound HTTPS** to reach image hosts. TLS root
certificates come from the bundled `webpki-roots`, so the image needs no CA
package.

Two deployment modes:
- **Mounted templates** (dev and most setups) use the compose file above, with hot reload.
- **Baked templates** (immutable prod) use `FROM rustypdf` and `COPY templates /templates`, with hot reload off.

---

## 11. Security
- API key auth (`Authorization: Bearer`). This is optional on a private network.
- Body size limit, render timeout and concurrency cap.
- Image fetcher limits:
  - SSRF guard (private-IP block with a check on every redirect);
  - optional host allowlist;
  - size, count and time caps;
  - magic-byte format check.
- Filesystem sandbox: templates can only read their template root, `/fonts`,
  `/packages` and request-scoped virtual files.
- Templates themselves have no network access.
- Runs as a non-root user with a read-only root filesystem.

---

## 12. Milestones

1. **M1 – Core render (MVP)**
   - Cargo project, `World` impl and font loading.
   - Compile one template from disk to a **PDF/A-3b** PDF in a unit test.
   - `POST /render/{template}`, registry scan and error mapping.
   - **Bangla + English** working, with the shared `lang.typ`, the bundled fonts
     and the per-script `covers` setup.
2. **M2 – Images + production basics**
   - URL image variables:
     - manifest pointers and parallel fetch;
     - SSRF guard and LRU cache;
     - `data:` URIs and the error policy.
   - JSON Schema validation, NFC normalization, auth, limits and timeouts.
   - `/healthz`, `/readyz`, `/metrics` and structured logs.
   - Dockerfile + compose.
   - 4 example bilingual invoice templates (retail, healthcare, logistics,
     construction), each with a `sample.json`.
3. **M3 – Quality and speed**
   - CI:
     - **veraPDF 3b** validation of every sample;
     - **golden PNG tests** for Bangla shaping;
     - image-fetch tests against a local mock server.
   - Benchmarks (`criterion` + `oha`/`k6`) and tuning.
   - Hot reload, `/preview`, `/templates` listing, offline Typst packages.
4. **M4 – Extras (optional)**
   - Batch endpoint (ZIP or merged PDF) and a PNG preview of page 1.
   - Client snippets (curl, TS, Python, PHP) and an OpenAPI spec (`utoipa`).

---

## 13. Calling it from another project

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
  body: JSON.stringify(invoice),   // image fields are plain public URLs
});
const pdf = Buffer.from(await res.arrayBuffer());
```

---

## 14. Decisions

| Topic | Decision |
|---|---|
| Bangla font | Noto Sans Bengali |
| Digits | No conversion. Bangla digits only where the request body or template contains them (see §4). |
| Image host allowlist | Optional. Set `IMAGE_ALLOWED_HOSTS` to a comma-separated list. When it is unset, any public host is allowed. |
| PDF/A-3 attachments | Not needed. The output is the PDF only. |

## 15. Open questions
1. Are Typst `@preview` packages needed? If so, which ones should be
   pre-bundled in the image?
