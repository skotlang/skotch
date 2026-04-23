class Greeter(val greet: () -> String) {
    fun run() {
        val msg = greet()
        println(msg)
    }
}
fun main() {
    val g = Greeter { "Hello from callable field!" }
    g.run()
}
