class Bag {
    val items: MutableList<String> = mutableListOf()
    fun add(s: String) { items.add(s) }
    fun show(): String = items.joinToString(",")
    fun size(): Int = items.size
}

fun main() {
    val b = Bag()
    b.add("apple")
    b.add("banana")
    b.add("cherry")
    println(b.show())
    println(b.size())
}
