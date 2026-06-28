fun main() {
    val xs = arrayOf("foo", "bar", "baz")
    println(xs.size)
    println(xs[0])
    println(xs[2])
    for (s in xs) println(s.uppercase())
    xs[1] = "QUX"
    println(xs[1])
}
