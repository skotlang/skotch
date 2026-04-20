fun main() {
    val greeting = "World"
    val greet = { name: String -> "Hello, $name!" }
    println(greet(greeting))
}
