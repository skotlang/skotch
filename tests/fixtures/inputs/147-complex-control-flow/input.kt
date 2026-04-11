fun collatz(n: Int): Int {
    var x = n
    var steps = 0
    while (x != 1) {
        if (x % 2 == 0) {
            x = x / 2
        } else {
            x = x * 3 + 1
        }
        steps += 1
    }
    return steps
}

fun main() {
    for (n in 1..10) {
        println("collatz($n) = ${collatz(n)}")
    }
}
