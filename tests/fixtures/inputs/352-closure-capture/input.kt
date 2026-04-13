fun main() {
    val x = 10
    val addX = { n: Int -> n + x }
    println(addX(5))
    println(addX(32))
    val name = "World"
    val greetName = { prefix: String -> prefix + ", " + name }
    println(greetName("Hello"))
}
