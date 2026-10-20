// Construction running bill (চলতি বিল): bill of quantities, VAT, retention,
// advance adjustment and net payable.
#import "/_shared/invoice.typ": *
#let data = json("/data.json")
#let accent = rgb("#b4780a")

#show: invoice-page.with(
  title: "Running bill " + data.bill_no,
  accent: accent,
  footer: [#data.contractor.name · #data.project.name],
)

#let pr = data.project
#invoice-header(
  logo: image("assets/logo.png", height: 1.3cm),
  seller: data.contractor,
  title: [চলতি বিল / Running bill],
  meta: (
    ([বিল নং / Bill no.], data.bill_no),
    ([তারিখ / Date], data.date),
    ([কার্যাদেশ / Work order], pr.work_order),
  ),
  accent: accent,
)

#parties(
  ([প্রকল্প / Project], [
    *#pr.name* \
    #if "site" in pr [সাইট / Site: #pr.site \ ]
    #if "period" in pr [সময়কাল / Period: #pr.period]
  ]),
  ([মালিক / Client], [
    *#data.client.name* \
    #data.client.at("address", default: "")
  ]),
  accent: accent,
)

// Amounts print in the digit system of the rate they came from.
#let ref = data.items.first().rate
#let amount(i) = num(i.qty) * num(i.rate)
#let work = data.items.map(amount).sum()
#let pct(key) = num(data.at(key, default: 0))
#let vat = work * pct("vat_percent") / 100
#let gross = work + vat
#let retention = work * pct("retention_percent") / 100
#let advance = num(data.at("advance_adjustment", default: 0))
#let net = gross - retention - advance

#items-table(
  columns: (auto, 1fr, auto, auto, auto, auto),
  align: (left, left, center, right, right, right),
  header: ([ক্রমিক / Item], [কাজের বিবরণ / Description of work], [একক / Unit], [পরিমাণ / Qty], [দর / Rate ৳], [মূল্য / Amount ৳]),
  rows: data.items.map(i => (
    i.at("code", default: ""),
    i.description,
    i.unit,
    i.qty,
    money(num(i.rate), i.rate),
    money(amount(i), i.rate),
  )),
  accent: accent,
)

#totals(
  (
    ([এই বিলে সম্পাদিত কাজ / Work this bill], money(work, ref)),
    ([ভ্যাট / VAT (#data.at("vat_percent", default: "0")%)], money(vat, ref)),
    ([মোট / Gross], money(gross, ref)),
    ([জামানত কর্তন / Retention (#data.at("retention_percent", default: "0")%)], [− #money(retention, ref)]),
    ..if advance != 0 { (([অগ্রিম সমন্বয় / Advance adjustment], [− #money(advance, ref)]),) },
    ([নিট প্রদেয় / Net payable], [৳ #money(net, ref)], true),
  ),
  accent: accent,
)

#if "previous_bills" in data {
  let prev = num(data.previous_bills)
  v(0.4em)
  text(8.5pt, fill: luma(60))[
    পূর্ববর্তী বিলসমূহ / Previous bills: ৳ #money(prev, data.previous_bills) ·
    ক্রমপুঞ্জিত কাজ / Cumulative work: ৳ #money(prev + work, data.previous_bills)
  ]
}

#signatures(
  (none, [ঠিকাদার / Contractor]),
  (none, [প্রকৌশলী / Site engineer]),
  (image("/_shared/assets/stamp.png", height: 1.9cm), [প্রকল্প পরিচালক / Project director]),
)
