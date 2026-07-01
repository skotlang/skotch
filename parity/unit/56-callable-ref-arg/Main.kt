fun <T, R> apply(x: T, f: (T) -> R): R = f(x)
fun square(n: Int): Int = n * n

fun main() {
    println(apply(7, ::square))
}
