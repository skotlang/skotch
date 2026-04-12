class Box(var value: Int) {
    fun add(n: Int) {
        value = value + n
    }
}

fun main() {
    val b = Box(10)
    b.add(5)
    b.add(3)
    println(b.value)
}
