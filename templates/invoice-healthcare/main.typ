// Healthcare bill (চিকিৎসা বিল): patient, services, discount, paid and due.
#import "/_shared/invoice.typ": *
#let data = json("/data.json")
#let accent = rgb("#14826e")

#show: invoice-page.with(
  title: "Medical bill " + data.bill_no,
  accent: accent,
  footer: [#data.clinic.name · জরুরি / Emergency: #data.clinic.at("phone", default: "")],
)

#invoice-header(
  logo: image("assets/logo.png", height: 1.3cm),
  seller: data.clinic,
  title: [বিল / Medical bill],
  meta: (
    ([বিল নং / Bill no.], data.bill_no),
    ([তারিখ / Date], data.date),
  ),
  accent: accent,
)

#let p = data.patient
#parties(
  ([রোগী / Patient], [
    *#p.name* \
    #if "age" in p [বয়স / Age: #p.age]
    #if "sex" in p [ · #p.sex] \
    #if "id" in p [আইডি / ID: #p.id]
    #if "phone" in p [ · #p.phone]
  ]),
  ([চিকিৎসক / Referred by], [
    #data.at("doctor", default: [—]) \
    #if "department" in data [বিভাগ / Dept.: #data.department]
  ]),
  accent: accent,
)

// Amounts print in the digit system of the price they came from.
#let ref = data.services.first().price
#let line-total(s) = num(s.at("qty", default: 1)) * num(s.price)
#let subtotal = data.services.map(line-total).sum()
#let discount-percent = num(data.at("discount_percent", default: 0))
#let discount = subtotal * discount-percent / 100
#let net = subtotal - discount
#let paid = num(data.at("paid", default: 0))

#items-table(
  columns: (auto, auto, 1fr, auto, auto, auto),
  align: (center, left, left, center, right, right),
  header: ([\#], [কোড / Code], [সেবা / Service], [সংখ্যা / Qty], [মূল্য / Rate ৳], [মোট / Amount ৳]),
  rows: data.services.enumerate().map(((n, s)) => (
    digits-like(n + 1, ref),
    raw(s.at("code", default: "")),
    s.description,
    s.at("qty", default: digits-like(1, ref)),
    money(num(s.price), s.price),
    money(line-total(s), s.price),
  )),
  accent: accent,
)

#totals(
  (
    ([মোট / Total], money(subtotal, ref)),
    ..if discount != 0 {
      (([ছাড় / Discount (#data.discount_percent%)], [− #money(discount, ref)]),)
    },
    ([প্রদেয় / Net payable], [৳ #money(net, ref)], true),
    ([পরিশোধিত / Paid], money(paid, ref)),
    ([বকেয়া / Due], strong(money(net - paid, ref))),
  ),
  accent: accent,
)

#if "notes" in data [
  #v(0.6em)
  #block(
    width: 100%,
    inset: 7pt,
    radius: 3pt,
    stroke: 0.5pt + accent.lighten(40%),
    text(8.5pt)[*নির্দেশনা / Instructions:* #data.notes],
  )
]

#signatures(
  (none, [ক্যাশিয়ার / Cashier]),
  (
    image("/_shared/assets/stamp.png", height: 2cm),
    [কর্তৃপক্ষ / Authority],
  ),
)
