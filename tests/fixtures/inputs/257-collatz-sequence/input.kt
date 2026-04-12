fun collatz(n: Int): Int {
    var count = 0
    var x = n
    while (x != 1) {
        if (x % 2 == 0) {
            x = x / 2
        } else {
            x = x * 3 + 1
        }
        count = count + 1
    }
    return count
}

fun main() {
    println(collatz(27))
    println(collatz(1))
    println(collatz(6))
}
