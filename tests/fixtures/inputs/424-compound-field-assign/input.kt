class Counter(var n: Int)

fun main() {
    val c = Counter(10)
    c.n += 5
    c.n -= 2
    c.n *= 3
    println(c.n)
}
