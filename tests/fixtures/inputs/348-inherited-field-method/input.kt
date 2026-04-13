open class A(val x: Int) {
    open fun f(): Int = x
}

class B(val y: Int) : A(y + 1) {
    override fun f(): Int = x + y
}

fun main() {
    val b = B(10)
    println(b.x)
    println(b.y)
    println(b.f())
}
