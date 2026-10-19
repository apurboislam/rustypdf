// Minimal template used by tests and as a smoke check.
#let data = json("/data.json")

#set document(title: "Hello " + data.at("name", default: "world"))
#set page(paper: "a4", margin: 2cm)
#set text(font: "Inter", size: 12pt)

= Hello, #data.at("name", default: "world")!

#if "message" in data [ #data.message ]
