fun apply(f: (Int) -> Int, x: Int): Int = f(x)
fun applyStr(f: (String) -> String, x: String): String = f(x)

fun main() {
    val doubled = apply({ it * 2 }, 21)
    println(doubled)
    val greeting = applyStr({ s: String -> "Hello, $s!" }, "world")
    println(greeting)
}
