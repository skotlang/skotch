class Counter(val initial: Int) {
    fun current(): Int = initial
}

suspend fun get(c: Counter): Int = c.current()
