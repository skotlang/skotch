class Box {
    var content: String? = null
    fun set(s: String) { content = s }
    fun get(): String? = content
}

fun main() {
    val b = Box()
    println(b.get())
    b.set("hello")
    println(b.get())
}
