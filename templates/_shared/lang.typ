// Shared Bangla + English setup. Import with:
//   #import "/_shared/lang.typ": setup, num, digits-like, bn
//   #show: setup

#let bn-font = "Noto Sans Bengali"
#let en-font = "Inter"

// Bengali block plus ZWNJ/ZWJ, which steer conjunct formation.
#let bn-covers = regex("[\u{0980}-\u{09FF}\u{200C}\u{200D}]")

// Bangla characters always use the Bangla font, everything else Inter,
// even inside one sentence.
#let setup(body) = {
  set text(
    font: ((name: bn-font, covers: bn-covers), en-font),
    size: 10pt,
    // No reliable Bangla hyphenation patterns; never break words.
    hyphenate: false,
  )
  body
}

#let bn-digits = "০১২৩৪৫৬৭৮৯".clusters()

// "১২৩৪.৫০", "1,234.50" or 1234.5 → a number for arithmetic.
#let num(v) = if type(v) == str {
  float(v.clusters().map(c => {
    let i = bn-digits.position(d => d == c)
    if i != none { str(i) } else { c }
  }).join().replace(",", ""))
} else { v }

// Prints `n` with Bangla digits only if `ref` (the input it was computed
// from) used Bangla digits; otherwise with ASCII digits.
#let digits-like(n, ref) = {
  let s = str(n)
  if type(ref) == str and ref.clusters().any(c => c in bn-digits) {
    s.clusters().map(c => if c >= "0" and c <= "9" { bn-digits.at(int(c)) } else { c }).join()
  } else { s }
}

// A Bangla paragraph, so line breaking uses Bangla rules.
#let bn(body) = text(lang: "bn", body)
