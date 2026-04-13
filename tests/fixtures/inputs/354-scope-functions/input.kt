fun main() {
    val upper = "hello".let { s: String -> s.uppercase() }
    println(upper)
    val x = 42.also { n: Int -> println("also: " + n) }
    println(x)
    val msg = "world".run { s: String -> "Hello, " + s }
    println(msg)
    val y = 10.apply { n: Int -> println("apply: " + n) }
    println(y)
    val w = with("kotlin") { s: String -> s.uppercase() }
    println(w)
}
