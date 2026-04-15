fun apply(f: (Int) -> Int, x: Int): Int = f(x)

fun transform(f: (Int) -> Int, x: Int): Int = f(x)

fun combine(f: (Int) -> Int, g: (Int) -> Int, x: Int): Int = g(f(x))

fun main() {
    println(apply({ it * 2 }, 5))
    println(transform({ it + 1 }, 10))
    println(transform({ it * 3 }, 10))
    println(combine({ it + 1 }, { it * 2 }, 5))
}
