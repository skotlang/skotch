class Wrapper(val items: IntArray) {
    operator fun get(i: Int): Int = items[i]
    operator fun set(i: Int, v: Int) { items[i] = v }
}

fun main() {
    val w = Wrapper(intArrayOf(10, 20, 30))
    println(w[1])
    w[1] = 99
    println(w[1])
}
