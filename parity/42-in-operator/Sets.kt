// User class with `operator fun contains` — Kotlin's `x in set`
// and `x !in set` desugar to `set.contains(x)` and `!set.contains(x)`.
// Probes that the dispatch finds the USER class's contains
// method instead of routing to `java/util/Set.contains` via the
// stdlib's substring-match intrinsic (which mis-fires on any
// class name containing "Set", e.g. IntSet, MutableSet, etc.).

class IntSet {
    val items: MutableList<Int> = mutableListOf()
    fun add(x: Int) {
        if (x !in this) {
            items.add(x)
        }
    }
    operator fun contains(x: Int): Boolean {
        var i = 0
        while (i < items.size) {
            if (items[i] == x) return true
            i = i + 1
        }
        return false
    }
    fun size(): Int = items.size
}
