fun main() {
    val xs = mutableListOf(1, 2, 3, 4, 5)
    xs.add(0, 0)
    println(xs)
    xs[3] = 99
    println(xs)
    xs.removeAt(xs.size - 1)
    println(xs)
    val ys = xs.toMutableList()
    ys.sort()
    println(ys)
    println(xs)
    xs.clear()
    println(xs.size)
}
