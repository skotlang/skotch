// Regression: a method whose body is `while (true) { if (cond) return …; var = … }`
// was being stubbed by the backend's `has_use_before_def` checker.
// The check intersected entry_defs across ALL preds including the
// unreachable exit_block of the while (the end-of-method writeback
// chain), and the unreachable block's empty entry_defs poisoned the
// intersection at downstream join points — falsely flagging `this`
// (slot 0) as undefined and triggering a stub. Same shape independently
// caused a backend stackmap `full_frame(@N,{Top},{})` for slot 0.
//
// Fix: skip unreachable predecessors in both the use-before-def
// intersection AND the stackmap live-slot dataflow.
class Counter(val limit: Int) {
    var pos: Int = 0

    fun runUntil(): String {
        while (true) {
            if (pos >= limit) return "stopped at ${pos}"
            pos = pos + 1
        }
    }
}

fun main() {
    val c = Counter(3)
    println(c.runUntil())
}
