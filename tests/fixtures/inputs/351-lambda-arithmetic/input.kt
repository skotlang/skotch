fun main() {
    val double = { x: Int -> x * 2 }
    val greet = { name: String -> "Hi " + name }
    println(double(5))
    println(double(21))
    println(greet("World"))
    println(double(3) + 10)
}
