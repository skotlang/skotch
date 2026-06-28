fun describe(x: Int): String = "int:$x"
fun describe(x: String): String = "str:$x"
fun describe(x: Double): String = "dbl:$x"
fun describe(x: Boolean): String = "bool:$x"

fun main() {
    println(describe(7))
    println(describe("hi"))
    println(describe(3.14))
    println(describe(true))
}
