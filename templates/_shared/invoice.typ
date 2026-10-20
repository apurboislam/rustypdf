// Shared invoice layout for the sector invoice templates. Import with:
//   #import "/_shared/invoice.typ": *
// It re-exports the helpers of lang.typ.
#import "/_shared/lang.typ": setup, num, digits-like, money, bn

// Page, fonts and base styles. `title` becomes the PDF title (PDF/A
// metadata); `footer` is printed at the bottom of every page.
#let invoice-page(title: none, accent: rgb("#1f4e79"), footer: none, body) = {
  set document(title: title)
  set page(
    paper: "a4",
    margin: (x: 1.6cm, top: 1.4cm, bottom: 1.9cm),
    footer: context {
      set text(7.5pt, fill: luma(110))
      line(length: 100%, stroke: 0.4pt + luma(200))
      v(-0.4em)
      footer
      h(1fr)
      counter(page).display("1 / 1", both: true)
    },
  )
  show: setup
  set text(9.5pt)
  set par(leading: 0.55em)
  set table(stroke: none, inset: (x: 5pt, y: 4pt))
  show heading: set text(fill: accent)
  body
}

// Logo and seller on the left; document title and `meta` (an array of
// (label, value) pairs) on the right.
#let invoice-header(logo: none, seller: (:), title: [], meta: (), accent: rgb("#1f4e79")) = {
  grid(
    columns: (1fr, auto),
    column-gutter: 1.5em,
    {
      if logo != none { logo; v(0.3em) }
      text(11pt, weight: "bold", seller.at("name", default: ""))
      linebreak()
      set text(8.5pt, fill: luma(60))
      for key in ("address", "phone", "email") {
        if key in seller { seller.at(key); linebreak() }
      }
      if "bin" in seller [বিআইএন / BIN: #seller.bin]
    },
    align(right, {
      text(17pt, weight: "bold", fill: accent, title)
      v(0.2em)
      grid(
        columns: 2,
        column-gutter: 0.8em,
        row-gutter: 0.45em,
        align: (right, left),
        ..meta.map(((k, v)) => (text(fill: luma(90), k), strong(v))).flatten(),
      )
    }),
  )
  v(0.6em)
  line(length: 100%, stroke: 1.2pt + accent)
  v(0.4em)
}

// Side-by-side boxes, each a (title, body) pair, e.g. "Bill to" / "Ship to".
#let parties(..boxes, accent: rgb("#1f4e79")) = {
  let boxes = boxes.pos()
  grid(
    columns: (1fr,) * boxes.len(),
    column-gutter: 0.8em,
    ..boxes.map(((title, body)) => block(
      width: 100%,
      inset: 7pt,
      radius: 3pt,
      fill: accent.lighten(92%),
      {
        text(8pt, weight: "bold", fill: accent, upper(title))
        v(0.15em)
        body
      },
    )),
  )
  v(0.5em)
}

// The line-item table. `header` and every row are arrays of cells.
#let items-table(columns: (), header: (), rows: (), align: auto, accent: rgb("#1f4e79")) = {
  table(
    columns: columns,
    align: align,
    fill: (_, y) => if y == 0 { accent } else if calc.even(y) { luma(245) },
    table.header(..header.map(h => text(fill: white, weight: "bold", size: 8.5pt, h))),
    ..rows.flatten(),
  )
}

// Right-aligned totals; `rows` are (label, value) or (label, value, true)
// for an emphasized row.
#let totals(rows, accent: rgb("#1f4e79")) = {
  align(right, block(width: 55%, table(
    columns: (1fr, auto),
    align: (left, right),
    ..rows.map(r => {
      let strong-row = r.len() > 2 and r.at(2)
      if strong-row {
        (
          table.cell(fill: accent.lighten(88%), strong(r.at(0))),
          table.cell(fill: accent.lighten(88%), text(11pt, weight: "bold", r.at(1))),
        )
      } else {
        (r.at(0), r.at(1))
      }
    }).flatten(),
  )))
}

// Signature lines at the bottom; each entry is (image or none, caption).
#let signatures(..entries) = {
  let entries = entries.pos()
  v(1fr)
  grid(
    columns: (1fr,) * entries.len(),
    column-gutter: 2em,
    align: center + bottom,
    ..entries.map(((img, caption)) => {
      if img != none { img } else { v(1.2cm) }
      line(length: 80%, stroke: 0.5pt)
      text(8pt, caption)
    }),
  )
}
