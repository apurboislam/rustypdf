# Writing a template

A template is a folder under `TEMPLATES_DIR` (`/templates` in Docker,
`./templates` in the repo) that contains a `main.typ`. It is served at
`POST /render/{folder}`, or at the `route` its `manifest.toml` sets. The four
sector invoices (`invoice-retail`, `invoice-healthcare`, `invoice-logistics`,
`invoice-construction`) are complete examples to copy from.

## Folder layout

```
templates/
├── _shared/                 # not a template (leading "_"); importable by all
│   ├── lang.typ             # Bangla + English fonts, num(), digits-like(), money()
│   ├── invoice.typ          # invoice layout blocks used by the sector invoices
│   └── assets/              # static images shared by every template
│       ├── no-photo.png
│       └── stamp.png
└── my-invoice/
    ├── main.typ             # required: the entry point
    ├── schema.json          # optional: JSON Schema for the request body
    ├── manifest.toml        # optional: name, route, limits, dynamic image fields
    ├── sample.json          # optional: for /preview, the startup check, CI
    └── assets/              # static images for this template only
        └── logo.png
```

The Typst project root is the templates folder itself, so absolute paths
start there: `#import "/_shared/lang.typ": *`. Templates cannot read anything
outside it, and have no network access. Packages (`@preview/...`) are not
available.

## `main.typ`

The request body is the virtual file `/data.json`:

```typst
#import "/_shared/lang.typ": setup, num, digits-like, money, bn
#let data = json("/data.json")

#set document(title: "Invoice " + data.invoice_no)  // PDF/A metadata
#set page(paper: "a4", margin: 2cm)
#show: setup                                         // Bangla + English fonts

#image("assets/logo.png", width: 4cm)               // static file
= চালান / Invoice #data.invoice_no
#for item in data.items [
  #image(item.photo, width: 1.2cm) #item.name        // dynamic image field
]
```

Every output is PDF/A-3b. Typst rejects anything the standard forbids at
compile time, so a template that compiles produces a compliant file. Always
set `#set document(title: ...)`.

Use `data.at("key", default: ...)` for optional fields; a missing key is a
compile error (`500` with file, line and column).

## Bangla and English: `lang.typ`

`#show: setup` sets the font list: characters in U+0980–U+09FF (plus
ZWNJ/ZWJ) always use **Noto Sans Bengali**, everything else uses **Inter**,
even inside one sentence. It also turns hyphenation off. Use `bn[...]` around
a Bangla-only paragraph so line breaking uses Bangla rules.

**Digits are never converted.** Bangla digits (০–৯) and ASCII digits print
exactly as the request sent them. For values the template computes:

| Helper | Does |
|---|---|
| `num(v)` | Parses `"১২৩৪.৫০"`, `"1,234.50"` or `1234.5` into a number. |
| `digits-like(n, ref)` | Prints `n` with Bangla digits only if `ref` (the input it came from) used them. |
| `money(n, ref)` | Like `digits-like`, with two decimals and South Asian grouping: `12,34,567.50` or `১২,৩৪,৫৬৭.৫০`. |

```typst
// price "৪৫০" and qty "২" → "৯০০.০০"; price "450" → "900.00"
#money(num(i.qty) * num(i.price), i.price)
```

## `_shared/invoice.typ`

The sector invoices share these blocks (import with
`#import "/_shared/invoice.typ": *`, which also brings in the `lang.typ`
helpers):

| Block | Use |
|---|---|
| `invoice-page.with(title:, accent:, footer:)` | `#show:` rule: A4 page, fonts, footer with page numbers. |
| `invoice-header(logo:, seller:, title:, meta:)` | Logo and seller left; title and `(label, value)` pairs right. |
| `parties((title, body), ...)` | Side-by-side boxes (bill to, ship to, patient, ...). |
| `items-table(columns:, header:, rows:, align:)` | The line-item table, with a colored header and zebra rows. |
| `totals(((label, value), ..., (label, value, true)))` | Right-aligned totals; `true` marks the emphasized row. |
| `signatures((image or none, caption), ...)` | Signature lines at the bottom of the page. |

## Images: static vs dynamic

| | Static | Dynamic |
|---|---|---|
| Example | logo, stamp, watermark | product photos, signatures chosen per request |
| Lives in | `<template>/assets/` or `_shared/assets/` | the request (URL, `data:` URI or a file path) |
| In the template | `#image("assets/logo.png")` | `#image(data.items.at(0).photo)` |
| Declared in the manifest | no | yes, under `[images."<pointer>"]` |

Static images are loaded once (and on hot reload) and checked by their magic
bytes: PNG, JPEG, GIF, WebP or SVG. A file in `assets/` that is not an image is
dropped with an error log.

For dynamic fields the service replaces each value with a resolved image
before compiling, so the template always gets a path it can pass to
`image()`. A value can be:
- an `http(s)` URL: fetched (SSRF-guarded, optionally limited by
  `IMAGE_ALLOWED_HOSTS`), resized to `max_px` and cached;
- a `data:` URI: decoded inline;
- a file path into `assets/` (`assets/sign/manager.png`) or
  `_shared/assets/` (`/_shared/assets/stamp.png`);
- missing, `null` or `""`: the field's `fallback`, or left as is when the
  field is `optional`.

## `manifest.toml`

Everything is optional:

```toml
name = "Retail invoice"        # display name (default: folder name)
route = "invoice-retail"       # POST /render/{route} (default: folder name)
max_body_kb = 512              # overrides MAX_BODY_KB
timeout_ms = 5000              # overrides RENDER_TIMEOUT_MS

# JSON pointer to the field; [] means every item of an array.
[images."/items/[]/photo"]
max_px = 400                                # downscale the longest side
fallback = "/_shared/assets/no-photo.png"   # for missing, empty or failed values
on_error = "fallback"                       # or "fail" → 422 naming the field

[images."/signature"]
optional = true                             # may be absent; left as is
on_error = "fail"
```

`on_error` defaults to `"fallback"` when a `fallback` is set, else `"fail"`.
A fallback that is used because loading failed is reported in the
`X-Render-Warnings` response header. Unknown keys, an invalid route, or a
fallback that is not an image under an `assets/` folder make the template
fail to load; it is then skipped (or, on hot reload, keeps its previous
version).

## `schema.json`

A JSON Schema (draft 2020-12 by default) for the request body, compiled once.
Invalid input gets a `422` before any image is fetched or anything compiled:

```json
{"error": "validation", "message": "invalid input",
 "details": [{"path": "/items/0/qty", "msg": "..."}]}
```

The sector invoices accept amounts as numbers or digit strings in either
system:

```json
"amount": { "anyOf": [
  { "type": "number", "minimum": 0 },
  { "type": "string", "pattern": "^[0-9০-৯][0-9০-৯,]*(\\.[0-9০-৯]+)?$" }
]}
```

## `sample.json` and checks

- At startup (and on reload) each template's sample is validated against its
  schema and compiled, with image URLs replaced by their fallback. Problems
  show up in `GET /templates/{route}` (`"ok": false`, `"errors": [...]`).
- `GET /preview/{route}` renders the sample (disable with `PREVIEW=false`).
- `just pdfa` renders every sample, with sample photo URLs
  (`http://cdn.rustypdf.test/...`) served from `tests/fixtures/photos`, and
  checks each PDF with veraPDF 3b.
- Add a golden test for a new template in `tests/golden.rs`
  (`check_golden("my-invoice")`), then approve its image with
  `just golden-update`. A test fails if a template with a sample has no
  golden test.
- Render one file by hand:
  `cargo run -- render templates/my-invoice templates/my-invoice/sample.json -o out.pdf`.
