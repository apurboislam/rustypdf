# Performance

What rustypdf costs per render, how it was measured, and what was tuned in
P8 (BUILD.md). Targets (PLAN.md §2):

| Target | Result | Met |
|---|---|---|
| Warm 1–2 page invoice, p50 < 15 ms on one core | New data every request: 13.4–14.6 ms render for retail, healthcare and logistics, 17.0 ms for construction; 16.4 ms through HTTP in a 1-CPU container (four templates in turn). Same data repeated: 2.2–2.7 ms | Partly: 3 of 4 templates in process; over HTTP, 1.4 ms over |
| Warm, p99 < 50 ms on one core | 29–38 ms render in process; 46.7 ms through HTTP in a 1-CPU container | Yes |
| Cold start < 1 s | 83–314 ms, process start to listening (fonts + template scan + one test compile per template) | Yes |
| Memory flat over a 30-minute load test | 262–293 MiB from minute 1.5 to minute 30, at 110 req/s (198k requests, 0 errors) | Yes |

## Hardware

All numbers come from one cloud VM: 4 vCPUs of an Intel Xeon @ 2.10 GHz,
16 GB RAM, Ubuntu 24.04, Docker 29, Rust 1.97 (release profile: fat LTO,
`codegen-units = 1`). It is a shared VM: back-to-back runs differ by about
±1 ms at p50, so only differences larger than that are reported as real.
The load generator (k6) ran on the same machine as the container.

## Results

### Render only (criterion, `just bench`)

`render::render` (compile + PDF/A-3b export) with images already resolved,
one thread:

| Template | Same data (`warm/*`) | New invoice number every iteration (`new-data/*`) |
|---|---|---|
| invoice-retail | 2.2 ms | 13.7 ms |
| invoice-healthcare | 2.4 ms | 14.6 ms |
| invoice-logistics | 2.4 ms | 13.4 ms |
| invoice-construction | 2.7 ms | 17.0 ms |

"Same data" is the
case where comemo replays almost everything; "new data" is what production
sees. Changing every text field instead of only the invoice number costs
about 0.6 ms more, so the per-request cost is mostly fixed Typst work
(layout, shaping, introspection) rather than proportional to what changed.
Of a new-data render, about 12 ms is compile and 3 ms is PDF export.

| Other benchmarks | Time |
|---|---|
| `images/warm`: retail photos (6 URLs) from the fetcher cache | 0.24 ms |
| `images/cold`: the same with cache misses (local mock server, decode, resize 640 → 400 px, JPEG re-encode), 4 threads | 20 ms (31 ms with Lanczos3) |
| `startup/fonts`: scan and index the 26 bundled faces | 3.5 ms |
| `startup/templates`: scan `templates/` and test-compile every sample, empty memo cache | 69 ms |

### In process over time (`just soak`)

`examples/soak.rs` renders the four sector invoices in turn, each with a new
invoice number, and evicts exactly like the service. One thread, 30 s:
p50 13.3–14.3 ms, p99 29–35 ms, RSS flat at about 195 MB.

### Through HTTP, in Docker (`just loadtest`)

`scripts/loadtest.sh` starts the image with `--read-only --cpus=N`,
`MAX_CONCURRENCY=N`, and runs `scripts/loadtest.js` (k6). Every request has
a new invoice number. 30 s per run:

| Scenario | CPUs / VUs | Throughput | HTTP p50 / p99 | Render p50 / p99 (`X-Render-Time-Ms`) | Memory at end |
|---|---|---|---|---|---|
| `warm`: 4 sector invoices in turn | 1 / 1 | 53 req/s | 16.4 / 46.7 ms | 15 / 42 ms | 197 MB |
| `warm` | 4 / 8 | 151 req/s | 51 / 88 ms | 24 / 54 ms | 330 MB |
| `cold`: retail, 5 photos as `data:` URIs (decoded and resized every request) | 1 / 1 | 13.5 req/s | 68 / 114 ms | 15 / 47 ms | 209 MB |

With 4 CPUs and 8 VUs, requests queue for a compile slot, and the 4
parallel renders share 4 hyperthreads with k6, so per-render time rises. The
`cold` scenario is mostly image work: per 640 px JPEG, about 0.9 ms decode,
5 ms resize and 3 ms JPEG re-encode. URL images pay this once, then come
from the cache (`images/warm`). The network part of a cold URL image is
bounded by `IMAGE_FETCH_TIMEOUT_MS` and runs before the compile slot is
taken. It cannot be load-tested against the container, because the SSRF
guard (rightly) refuses every local mock server, so `images/cold` measures
it in process.

### 30-minute soak (`just loadtest soak`)

`CPUS=2 VUS=4 just loadtest soak`: the `warm` scenario for 30 minutes,
with container memory (`docker stats`) sampled every 30 s.

- 197,792 requests at 110 req/s; every response was a 200.
- HTTP p50 / p99 34 / 63 ms (4 VUs on 2 slots, so requests queue); render
  p50 / p99 16 / 38 ms.
- Memory: 52 MiB after start, 224 MiB after the first seconds, then
  **262–293 MiB for the rest of the 30 minutes** with no upward trend
  (minute 2: 283 MiB; minute 15: 278 MiB; minute 30: 283 MiB; at the end:
  278 MiB).

Before P8 (timer eviction), the same kind of load grew memory by about
60 MB/s; see below.

## What was tuned

### Memo cache eviction (the big one)

Typst memoizes parsing, evaluation, layout and shaping in global `comemo`
caches, which only `comemo::evict(max_age)` shrinks. Before P8 the service
called `evict(30)` every 10 s, so entries lived about 5 minutes. With new
data every request, most entries are never reused, and they piled up at
about 1.7 MB per render:

| Policy (1 thread, new data) | p50 | p99 | RSS after 30–40 s |
|---|---|---|---|
| `evict(30)` every 10 s (before) | 18.8 ms | 156 ms | 2.6 GB and growing ~60 MB/s |
| `evict(5)` every 1 s | 15.7 ms | 68 ms | 763 MB |
| `evict(1)` every 100 ms | 16.3 ms | 33 ms | 169 MB |
| `evict(0)` after every render (no reuse at all) | 40.3 ms | 87 ms | 166 MB |
| **`evict(1)` every 16 renders (now)** | **14–15 ms** | **31–37 ms** | **~195 MB, flat** |

A big cache is slower as well as larger (hashing into huge maps, page
faults). Time-based eviction lets memory grow with throughput, so the
service now evicts every `MEMO_EVICT_EVERY` renders (`src/memo.rs`), plus a
30 s timer that only fires when no render happened, to release memory
after a burst. Between 4 and 64 renders the differences were within noise.
The constraint is the number of templates in rotation: an entry survives
`N` to `2N` renders, so with `N` below the number of templates in regular
use, each template's shared work (imports, fixed text) is evicted before
its next request (`N` = 1–2 with 4 templates: p50 19–20 ms). 16 leaves room
for more templates. Raise it if you serve many templates with even traffic.

### Allocator: mimalloc

Under glibc malloc, `malloc`/`free` and page faults from returning memory to
the OS took about 20% of a render. With mimalloc (`src/lib.rs`), new-data
renders went from 17.6–18.5 ms to 14.9–15.7 ms p50 (A/B of the same build),
and same-data renders improved by 10–16%. RSS is about 75 MB higher.
mimalloc's purge-delay and huge-page options made no difference beyond
noise, so they are left at their defaults.

### Release profile: fat LTO, one codegen unit

About 7% more throughput and roughly 1 ms off p50 (A/B over three
alternating 20 s soaks: p50 13.3–14.3 ms vs 14.5–15.3 ms). The cost is a
slower release and Docker build.

### Image resize: `thumbnail` instead of Lanczos3

Downscaling a 640 px photo took 9.6 ms with Lanczos3. `image`'s
`thumbnail` (area averaging, designed for downscaling) takes 5 ms, and its
output differs from Lanczos3 by 0.26/255 per channel on average. The golden
images are unchanged within their tolerance.

### Checked and left alone

- **Font loading:** 3.4 ms at startup, parsed lazily per face; nothing to gain.
- **JSON conversion:** `serde_json` is under 0.2% of a render. The request
  JSON is now serialized once per render instead of twice.
- **Page-count footer** (`counter(page).display(both: true)` needs a second
  layout pass): removing it saved about 0.5 ms, not worth losing "1 / 2".
- **What remains** is Typst itself: text shaping (rustybuzz + ttf-parser,
  about 25%), layout and styles (about 25%), comemo bookkeeping (about 8%),
  and zlib compression in PDF export (about 6%).

### Defaults

| Setting | Default | Why |
|---|---|---|
| `MAX_CONCURRENCY` | CPU cores | Renders are CPU-bound. More slots than cores only adds contention (the 4-CPU run above already shows per-render time rising). |
| `MEMO_EVICT_EVERY` | 16 | See above. |
| `IMAGE_CACHE_MB` | 256 | Resized product photos are 20–60 KB, so this holds thousands. Unchanged. |
| `QUEUE_TIMEOUT_MS` / `RENDER_TIMEOUT_MS` | 5000 / 10000 | Far above any measured render; they guard against overload and broken templates. Unchanged. |

## Reproducing

```bash
just bench                         # criterion; HTML reports in target/criterion
just soak 60 1                     # in process: latency + RSS every 5 s
MEMO_EVICT_EVERY=64 just soak      # try another eviction setting
SOAK_COMPILE_ONLY=1 just soak      # compile without PDF export
SOAK_VARY_ALL=1 just soak          # change every text field, not just the number
just docker && just loadtest warm  # k6 against the image (also: cold, soak)
CPUS=4 VUS=8 just loadtest warm
```

To profile, run any of these under `perf record -g` (or `cargo flamegraph`),
e.g. the bench binary that `cargo bench --no-run` prints, with
`--bench --profile-time 15 new-data/invoice-retail`.

CI (`bench` job) runs the criterion suite on every push. `main` saves its
results as the baseline (actions cache), and pull requests are compared
with it by `scripts/bench-ci.sh`. A benchmark whose median is more than 20%
slower (with the whole 95% confidence interval above 20%) gets a warning
annotation; the job never fails on timing, because shared runners are too
noisy to gate merges on.
