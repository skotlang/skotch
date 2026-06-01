class Binding(val name: String, val version: Int) {
    companion object {
        @JvmStatic fun inflate(name: String): Binding = inflate(name, 0)
        @JvmStatic fun inflate(name: String, version: Int): Binding {
            return Binding(name, version)
        }
    }
}

fun main() {
    val b = Binding.inflate("hello")
    println(b.name)
    println(b.version)
}
