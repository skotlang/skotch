fun main() {
    var s = 0
    for (i in 1..5) s += i
    println(s)
    for (i in 10 downTo 6) print("$i ")
    println()
    for (i in 0..10 step 2) print("$i ")
    println()
}
