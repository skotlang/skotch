class Service {
    lateinit var name: String

    fun setup(n: String) { name = n }
}

fun main() {
    val s = Service()
    s.setup("myService")
    println(s.name)
}
