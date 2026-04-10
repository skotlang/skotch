object Registry {
    private val items = mutableListOf<String>()
    fun add(item: String) { items.add(item) }
    fun all(): List<String> = items
}

fun main() {
    Registry.add("first")
    Registry.add("second")
    println(Registry.all())
}
