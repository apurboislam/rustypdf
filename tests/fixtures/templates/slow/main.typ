// Deliberately slow: loops `n` times (queue and timeout tests).
#let d = json("/data.json")
#let total = 0
#for i in range(d.n) { total += i }
#total
