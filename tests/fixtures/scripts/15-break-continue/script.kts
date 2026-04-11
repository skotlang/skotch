var sum = 0
for (i in 1..20) {
    if (i % 2 == 0) {
        continue
    }
    sum += i
    if (sum > 30) {
        break
    }
}
println(sum)
