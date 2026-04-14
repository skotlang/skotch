fun main() {
    val transform: (Int) -> Int = { x: Int -> x * 3 }
    println(transform(7))
    val greet: (String) -> String = { name: String -> "Hi " + name }
    println(greet("World"))
}
