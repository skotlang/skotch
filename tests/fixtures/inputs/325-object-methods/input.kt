object MathUtils {
    fun square(x: Int): Int = x * x
    fun cube(x: Int): Int = x * x * x
}

fun main() {
    println(MathUtils.square(5))
    println(MathUtils.cube(3))
}
