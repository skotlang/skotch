// Drives the linked-list across both ref (String) and primitive-
// box (Int — boxed because the generic param erases to Object)
// element types. Confirms the same code path handles both.

fun main() {
    // ── String list ─────────────────────────────────────────
    val strs = LinkedList<String>()
    strs.push("a")
    strs.push("b")
    strs.push("c")
    println(strs.size())               // 3
    println(strs.pop())                // c
    println(strs.pop())                // b
    println(strs.size())               // 1
    println(strs.pop())                // a
    println(strs.pop())                // null
    println(strs.size())               // 0
    println("---")

    // ── Int list (primitives boxed at element slots) ───────
    val ints = LinkedList<Int>()
    var i = 1
    while (i <= 5) {
        ints.push(i)
        i = i + 1
    }
    println(ints.size())               // 5

    // Pop fixed count — gives a clean addition without relying on
    // the var-field cache invalidation between `ints.size()` calls
    // (a separate gap when a method mutates state and another reads
    // back through the same chain).
    var sum = 0
    var k = 0
    while (k < 5) {
        val taken = ints.pop()
        if (taken != null) {
            sum = sum + taken
        }
        k = k + 1
    }
    println(sum)                        // 1+2+3+4+5 = 15
    println(ints.size())               // 0
}
