fun safe(n: Int): Int {
    if (n < 0) throw IllegalArgumentException("neg")
    return n
}

fun guard(x: Int): String {
    if (x >= 0) return x.toString()
    else throw IllegalArgumentException("negative")
}

fun main() {
    println(safe(5))
    println(guard(42))
}
