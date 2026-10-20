// Retail invoice (খুচরা চালান) with product photos from URLs.
#import "/_shared/invoice.typ": *
#let data = json("/data.json")
#let accent = rgb("#c8462a")

#show: invoice-page.with(
  title: "Invoice " + data.invoice_no,
  accent: accent,
  footer: [#data.seller.name · ধন্যবাদ / Thank you for shopping with us],
)

#invoice-header(
  logo: image("assets/logo.png", height: 1.3cm),
  seller: data.seller,
  title: [চালান / Invoice],
  meta: (
    ([চালান নং / No.], data.invoice_no),
    ([তারিখ / Date], data.date),
    ..if "payment" in data { (([পরিশোধ / Payment], data.payment),) },
  ),
  accent: accent,
)

#parties(
  ([গ্রাহক / Billed to], [
    *#data.customer.name* \
    #data.customer.at("address", default: "") \
    #data.customer.at("phone", default: "")
  ]),
  accent: accent,
)

// Amounts print in the digit system of the price they came from.
#let ref = data.items.first().price
#let line-total(i) = num(i.qty) * num(i.price)
#let subtotal = data.items.map(line-total).sum()
#let discount = num(data.at("discount", default: 0))
#let vat-percent = num(data.at("vat_percent", default: 0))
#let vat = (subtotal - discount) * vat-percent / 100

#items-table(
  columns: (auto, 1.4cm, 1fr, auto, auto, auto),
  align: (center + horizon, center + horizon, left + horizon, right + horizon, right + horizon, right + horizon),
  header: ([\#], [ছবি / Photo], [পণ্য / Item], [পরিমাণ / Qty], [দর / Price ৳], [মোট / Total ৳]),
  rows: data.items.enumerate().map(((n, i)) => (
    str(n + 1),
    box(image(i.photo, width: 1.2cm, height: 1.2cm, fit: "contain")),
    i.name,
    [#i.qty #i.at("unit", default: "")],
    money(num(i.price), i.price),
    money(line-total(i), i.price),
  )),
  accent: accent,
)

#totals(
  (
    ([উপমোট / Subtotal], money(subtotal, ref)),
    ..if discount != 0 { (([ছাড় / Discount], [− #money(discount, ref)]),) },
    ..if vat-percent != 0 {
      (([ভ্যাট / VAT (#data.vat_percent%)], money(vat, ref)),)
    },
    ([সর্বমোট / Grand total], [৳ #money(subtotal - discount + vat, ref)], true),
  ),
  accent: accent,
)

#if "notes" in data [
  #v(0.6em)
  #text(8.5pt, fill: luma(70))[*নোট / Notes:* #data.notes]
]

#signatures(
  (none, [গ্রাহকের স্বাক্ষর / Customer signature]),
  (
    {
      place(dx: 2.4cm, dy: -0.6cm, image("/_shared/assets/stamp.png", height: 2.2cm))
      if data.at("signature", default: none) != none {
        image(data.signature, height: 1.2cm)
      } else { v(1.2cm) }
    },
    [অনুমোদিত স্বাক্ষর / Authorized signature],
  ),
)
