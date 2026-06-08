// Second Comparable impl to confirm cross-file synthesis. Items
// compare by `priority` DESCENDING (high priority first), so the
// sorted-list output is the reverse of the natural numeric order.

class Item(val tag: String, val priority: Int) : Comparable<Item> {
    override fun compareTo(other: Item): Int = other.priority - priority
    override fun toString(): String = "${tag}[${priority}]"
}
