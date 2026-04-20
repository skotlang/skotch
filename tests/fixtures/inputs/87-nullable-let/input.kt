fun main() {
    val name = "world"
    val result = name.let { s: String -> "Hello, $s!" }
    println(result)
}
