class MathUtils {
    companion object {
        fun square(n: Int): Int = n * n
        fun cube(n: Int): Int = n * n * n
    }
}
fun main() {
    println(MathUtils.square(5))
    println(MathUtils.cube(3))
}
