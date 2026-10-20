// Logistics invoice (পরিবহন চালান): shipment, route, charges, COD.
#import "/_shared/invoice.typ": *
#let data = json("/data.json")
#let accent = rgb("#2850aa")

#show: invoice-page.with(
  title: "Freight invoice " + data.invoice_no,
  accent: accent,
  footer: [#data.company.name · ট্র্যাক করুন / Track: #data.shipment.tracking_no],
)

#let s = data.shipment
#invoice-header(
  logo: image("assets/logo.png", height: 1.3cm),
  seller: data.company,
  title: [পরিবহন চালান / Freight invoice],
  meta: (
    ([চালান নং / No.], data.invoice_no),
    ([তারিখ / Date], data.date),
    ([ট্র্যাকিং / Tracking], s.tracking_no),
  ),
  accent: accent,
)

// Route strip: origin → destination with the shipment facts.
#block(width: 100%, inset: 8pt, radius: 3pt, fill: accent.lighten(92%), grid(
  columns: (1fr, auto, 1fr),
  align: (left + horizon, center + horizon, right + horizon),
  [#text(7.5pt, fill: luma(90))[প্রেরণ স্থান / ORIGIN] \ #text(12pt, weight: "bold", s.origin)],
  text(18pt, fill: accent)[⟶],
  [#text(7.5pt, fill: luma(90))[গন্তব্য / DESTINATION] \ #text(12pt, weight: "bold", s.destination)],
))
#v(0.3em)
#grid(
  columns: (1fr,) * 4,
  column-gutter: 0.6em,
  ..(
    ([মাধ্যম / Mode], s.at("mode", default: "—")),
    ([সেবা / Service], s.at("service", default: "—")),
    ([প্যাকেজ / Pieces], str(s.at("pieces", default: "—"))),
    ([ওজন / Weight], [#s.at("weight_kg", default: "—") kg]),
  ).map(((k, v)) => block(width: 100%, inset: 6pt, stroke: 0.5pt + luma(210), radius: 3pt)[
    #text(7.5pt, fill: luma(90), k) \ *#v*
  ]),
)
#v(0.4em)

#let party(p) = [
  *#p.name* \
  #p.at("address", default: "") \
  #p.at("phone", default: "")
]
#parties(
  ([প্রেরক / Consignor], party(data.consignor)),
  ([প্রাপক / Consignee], party(data.consignee)),
  accent: accent,
)

// Amounts print in the digit system of the first charge.
#let ref = data.charges.first().amount
#let subtotal = data.charges.map(c => num(c.amount)).sum()
#let vat-percent = num(data.at("vat_percent", default: 0))
#let vat = subtotal * vat-percent / 100

#items-table(
  columns: (auto, 1fr, auto),
  align: (center, left, right),
  header: ([\#], [বিবরণ / Description], [পরিমাণ / Amount ৳]),
  rows: data.charges.enumerate().map(((n, c)) => (
    str(n + 1),
    c.description,
    money(num(c.amount), c.amount),
  )),
  accent: accent,
)

#totals(
  (
    ([উপমোট / Subtotal], money(subtotal, ref)),
    ..if vat-percent != 0 {
      (([ভ্যাট / VAT (#data.vat_percent%)], money(vat, ref)),)
    },
    ([মোট ভাড়া / Total charges], [৳ #money(subtotal + vat, ref)], true),
    ..if "cod_amount" in data {
      (([পণ্যমূল্য সংগ্রহ / Cash on delivery], [৳ #money(num(data.cod_amount), data.cod_amount)]),)
    },
  ),
  accent: accent,
)

#if "notes" in data [
  #v(0.6em)
  #text(8.5pt, fill: luma(70))[*শর্তাবলি / Terms:* #data.notes]
]

#signatures(
  (none, [প্রেরকের স্বাক্ষর / Consignor]),
  (none, [প্রাপকের স্বাক্ষর / Consignee]),
  (image("/_shared/assets/stamp.png", height: 1.9cm), [অনুমোদিত / Authorized]),
)
