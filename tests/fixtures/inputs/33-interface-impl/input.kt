// TODO: interface declarations + implementations.
interface Greeter {
    fun greet()
}

class Hello : Greeter {
    override fun greet() {
        println("hi")
    }
}
