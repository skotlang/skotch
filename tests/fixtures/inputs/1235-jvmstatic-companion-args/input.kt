// @JvmStatic on a companion method with arguments. The synthesized
// static delegate on the outer class must forward the args through to
// the companion's instance method.

class Math {
    companion object {
        @JvmStatic
        fun double(x: Int): Int = x * 2

        @JvmStatic
        fun add(a: Int, b: Int): Int = a + b
    }
}

fun main() {
    println(Math.double(21))
    println(Math.add(7, 35))
}
