class MathUtils {
    companion object {
        @JvmStatic
        fun square(n: Int): Int = n * n
    }
}

fun main() {
    println(MathUtils.square(5))
}
