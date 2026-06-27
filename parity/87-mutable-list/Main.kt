fun main() {
    val xs = mutableListOf<Int>()
    for (i in 1..5) xs.add(i * i)
    xs.removeAt(0)
    xs[1] = 99
    println(xs)
    println(xs.size)
    println(xs.sum())
}
