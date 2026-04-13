fun main() {
    val double = { x: Int -> x * 2 }
    println(double(5))
    println(double(21))
    val greet = { name: String -> "Hello, " + name }
    println(greet("World"))
    val add = { a: Int, b: Int -> a + b }
    println(add(3, 4))
}
