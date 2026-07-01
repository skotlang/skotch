fun indent(d: Int): String {
    var s = ""
    var i = 0
    while (i < d) {
        s += "  "
        i++
    }
    return s
}

fun main() {
    // Build a small tree with the DSL.
    val t = Tree.of("root") {
        child("alpha") {
            leaf("alpha-1")
            child("alpha-2") {
                leaf("alpha-2-a")
            }
        }
        child("beta") {
            leaf("beta-1")
        }
        leaf("gamma")
    }

    // Pre-order walk with depth threaded into the callback.
    t.walk({ v, d ->
        println("${indent(d)}$v")
    })

    // Show the static node count by using the same walk to thread a
    // counter through a sentinel field instead of capturing a `var` —
    // skotch's lambda-capture path for `var` rewrites still has a
    // VerifyError on the autoboxed-write back, separate from the
    // lambda-with-receiver fix landing in this iteration.
    println("--- done ---")
}
