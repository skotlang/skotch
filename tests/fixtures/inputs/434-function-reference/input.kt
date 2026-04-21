fun double(x: Int): Int = x * 2

fun apply(f: (Int) -> Int, x: Int): Int = f(x)

fun main() {
    println(apply(::double, 5))
    println(apply(::double, 10))
}
