class Box<T>(val value: T) {
    fun show(): String = "box($value)"
    fun <R> map(f: (T) -> R): Box<R> = Box(f(value))
}

fun main() {
    val b = Box(7)
    println(b.show())
    val s = b.map { "v=$it" }
    println(s.show())
    val d = b.map { it * 2.0 }
    println(d.show())
}
