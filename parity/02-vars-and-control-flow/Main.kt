fun main() {
    val name = "Kotlin"
    var counter = 0
    counter += 5
    counter--
    println("name=$name counter=$counter")

    val n = 12
    val parity = if (n % 2 == 0) "even" else "odd"
    println("$n is $parity")

    val rating = when {
        n < 5  -> "small"
        n < 10 -> "medium"
        else   -> "large"
    }
    println("$n is $rating")

    var sum = 0
    for (i in 1..5) sum += i
    println("sum(1..5) = $sum")

    var pow = 1
    var exp = 0
    while (pow < 100) { pow *= 2; exp++ }
    println("2^$exp = $pow")

    val r = (1..20)            // parenthesised range expression
    var first = -1
    for (i in r) {
        if (i % 7 == 0) { first = i; break }
    }
    println("first multiple of 7 in 1..20 = $first")
}
