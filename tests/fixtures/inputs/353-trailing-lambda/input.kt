fun main() {
    val action = { s: String -> println("got: " + s) }
    action("hello")
    action("world")
}
