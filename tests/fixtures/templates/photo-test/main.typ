// Static and dynamic images (image and manifest tests).
#let data = json("/data.json")
#set document(title: "Photos")
#set text(font: "Inter")

#image("assets/logo.png", width: 3cm)
= #data.title
#for item in data.at("items", default: ()) [
  #item.name: #box(image(item.photo, width: 1cm)) \
]
#if data.at("signature", default: none) != none {
  image(data.signature, width: 2cm)
}
#image("/_shared/assets/stamp.png", width: 1cm)
