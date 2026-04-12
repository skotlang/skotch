var x = 27
var count = 0
while (x != 1) {
    if (x % 2 == 0) {
        x = x / 2
    } else {
        x = x * 3 + 1
    }
    count = count + 1
}
println(count)
