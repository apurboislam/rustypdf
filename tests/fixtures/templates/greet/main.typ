#let data = json("/data.json")
#set document(title: "Greeting")
#set text(font: "Inter")
Hello, #data.at("name", default: "world")!
