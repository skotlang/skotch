class Counter(val value: Int) {
    companion object {
        fun zero(): Counter = Counter(0)
        fun of(v: Int): Counter = Counter(v)
    }
    fun next(): Counter = Counter(value + 1)
}

fun main() {
    val a = Counter.zero()
    val b = Counter.of(7)
    println(a.value)
    println(b.next().value)
}
