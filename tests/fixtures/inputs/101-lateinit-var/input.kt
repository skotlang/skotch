class Service {
    lateinit var name: String

    fun init(n: String) { name = n }
}

fun main() {
    val s = Service()
    s.init("myService")
    println(s.name)
}
