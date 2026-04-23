fun applyTwice(f: (Int) -> Int, x: Int): Int = f(f(x))
fun main() {
    println(applyTwice({ it + 1 }, 0))
    println(applyTwice({ it * 2 }, 3))
}
