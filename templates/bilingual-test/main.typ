// Shaping corpus for golden tests (BUILD.md P2). Not a real document.
#import "/_shared/lang.typ": setup, num, digits-like, bn
#let data = json("/data.json")

#set document(title: "Bilingual shaping test")
#set page(paper: "a5", margin: 1.5cm)
#show: setup

= বাংলা / English শিরোনাম Heading

== যুক্তাক্ষর (conjuncts)
#bn[ক্ষ জ্ঞ ন্ত্র ক্ষমা বিজ্ঞান মন্ত্রী স্বাস্থ্য]

== রেফ (reph) ও কার (vowel signs)
#bn[র্ক কর্ম ধর্ম — কি কী কো কৌ কে কৈ]

== খণ্ড ত ও চন্দ্রবিন্দু
#bn[ৎ উৎসব হঠাৎ — ঁ চাঁদ বাঁশি]

== মিশ্র বাক্য (mixed)
আমি Rust দিয়ে PDF বানাই, and it works ঠিকঠাক।

== অঙ্ক (digits, never converted)
#table(
  columns: 3,
  [*Input*], [*Qty × Price*], [*মোট / Total*],
  ..data.rows.map(r => (
    [৳ #r.price],
    [#r.qty × #r.price],
    [৳ #digits-like(num(r.qty) * num(r.price), r.price)],
  )).flatten()
)

ASCII stays ASCII: 0123456789 · বাংলা থাকে বাংলা: ০১২৩৪৫৬৭৮৯

#data.note
