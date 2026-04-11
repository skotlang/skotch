fun main() {
    for (n in 1..5) {
        val size = when {
            n <= 2 -> "small"
            n <= 4 -> "medium"
            else -> "large"
        }
        val parity = when {
            n % 2 == 0 -> "even"
            else -> "odd"
        }
        println("$n: $size, $parity")
    }
}
