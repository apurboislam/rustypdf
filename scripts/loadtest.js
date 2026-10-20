// k6 load test for a running rustypdf (driven by scripts/loadtest.sh).
//
// Every request gets a new invoice number, so no two bodies are equal: the
// production case, where comemo can reuse a template's fixed parts but never
// a whole document.
//
// Env: BASE_URL (default http://localhost:8080), BODIES (JSON file mapping
// route -> sample body, written by loadtest.sh), API_KEY (optional),
// VUS, DURATION.
import http from "k6/http";
import { check } from "k6";
import { Trend } from "k6/metrics";

const base = __ENV.BASE_URL || "http://localhost:8080";
const bodies = JSON.parse(open(__ENV.BODIES));
const routes = Object.keys(bodies);
const headers = { "Content-Type": "application/json" };
if (__ENV.API_KEY) headers.Authorization = `Bearer ${__ENV.API_KEY}`;

// Server-side compile + export time (X-Render-Time-Ms), without HTTP and
// image resolution.
const renderTime = new Trend("render_time_ms", true);

export const options = {
  vus: Number(__ENV.VUS || 1),
  duration: __ENV.DURATION || "30s",
  summaryTrendStats: ["avg", "p(50)", "p(90)", "p(99)", "max"],
  discardResponseBodies: true,
};

export default function () {
  const route = routes[(__VU + __ITER) % routes.length];
  const body = Object.assign({}, bodies[route]);
  const field = "invoice_no" in body ? "invoice_no" : "bill_no";
  body[field] = `LT-${__VU}-${__ITER}`;
  const res = http.post(`${base}/render/${route}`, JSON.stringify(body), {
    headers,
    tags: { route },
  });
  check(res, { "status 200": (r) => r.status === 200 });
  const ms = Number(res.headers["X-Render-Time-Ms"]);
  if (!Number.isNaN(ms)) renderTime.add(ms, { route });
}
