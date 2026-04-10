class Outer(val x: Int) {
    inner class Inner(val y: Int) {
        fun sum(): Int = x + y
    }
}

fun main() {
    val outer = Outer(10)
    val inner = outer.Inner(5)
    println(inner.sum())
}
