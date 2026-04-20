fun apply(f: (Int) -> Int, x: Int): Int = f(x)

fun main() {
    println(apply({ n: Int -> n * 2 }, 5))
    println(apply({ n: Int -> n + 10 }, 3))
}
