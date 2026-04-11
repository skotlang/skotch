class Box(val value: Int)

fun unbox(b: Box): Int = b.value
fun doubleBox(b: Box): Int = b.value * 2

fun main() {
    val b = Box(21)
    println(unbox(b))
    println(doubleBox(b))
}
