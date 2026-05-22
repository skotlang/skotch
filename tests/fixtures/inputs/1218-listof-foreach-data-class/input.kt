// Locks in: `list.forEach { ... }` where `it` accesses both
// a String field and an Int field of the element class — verifies
// that the element type's full member set is reachable from the
// lambda, not just one field type.

data class Item(val label: String, val count: Int)

fun main() {
    val items = listOf(
        Item("apples", 3),
        Item("oranges", 7),
    )
    items.forEach {
        println(it.label + ":" + it.count.toString())
    }
}
