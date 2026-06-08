// User class with `operator fun contains` — Kotlin's `x in obj`
// and `x !in obj` desugar to `obj.contains(x)` and `!obj.contains(x)`.
// The class name (`IntSet`) contains the "Set" substring that
// triggered mir-lower's `Map/List/Set` intrinsic dispatches at
// ~9645/~9764/~9902, routing to `java/util/Set.contains` → IncompatibleClassChangeError.
//
// Fix at mir-lower:~9644 computes `user_class_has_intrinsic` for
// the receiver class + method-name pair, then gates all three
// substring-based collection intrinsic dispatches on the negation
// so user methods win. Same pattern as iteration 39's
// stdlib-extension-fallback gate (lib.rs:~9927/~11232), now extended
// to the intrinsic-collection dispatches.

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
}

fun main() {
    val s = IntSet()
    s.add(1)
    s.add(2)
    s.add(2)
    s.add(7)
    println(2 in s)
    println(5 in s)
    println(7 !in s)
    println(s.items.size)
}
