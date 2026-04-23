class Box(var value: Int) {
    fun update(n: Int) {
        value = n
    }
    fun doubleIt() {
        value = value * 2
    }
}

fun main() {
    val b = Box(10)
    println(b.value)
    b.update(42)
    println(b.value)
    b.doubleIt()
    println(b.value)
}
