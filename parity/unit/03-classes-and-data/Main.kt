fun main() {
    val origin = Point(0, 0)
    val p = origin.translate(3, 4)
    println("origin = $origin")
    println("p      = $p")

    val q = p.copy(y = 10)
    println("q (copy with y=10) = $q")
    println("p == q? ${p == q}")
    println("p == p.copy()? ${p == p.copy()}")

    val c = Counter()
    repeat(3) { c.bump() }
    println("counter after 3 bumps = ${c.value()}")

    val c2 = Counter(100)
    c2.bump()
    println("counter starting at 100, +1 = ${c2.value()}")
}
