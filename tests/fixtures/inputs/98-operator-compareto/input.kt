class Box(val size: Int) {
    operator fun compareTo(other: Box): Int = size - other.size
}

fun main() {
    val big = Box(10)
    val small = Box(3)
    println(big.compareTo(small) > 0)
    println(small.compareTo(big) < 0)
}
