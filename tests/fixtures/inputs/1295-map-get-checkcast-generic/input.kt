// Regression: `Map.get(k)` returns Object on the JVM after V erases.
// Without a generic-arg-aware lift, subsequent `.add` / `.size` /
// any non-Object method calls on the result silently dispatched
// against `Ty::Any` and dropped (the call instruction got POPped).
// Symptom on the bigger graph BFS example: `edges.get(from)!!.add(to)`
// did the get + pop and never called `.add`, so the graph stayed
// empty.
//
// Fix: the Map.get intrinsic emit looks up the receiver's
// `local_generic_args[1]` (V type), inserts a `CheckCast` to that
// class after the call, and uses the casted local as the dest. Also
// propagates nested generic args (V's own generics) so a follow-up
// `.get(i)` on the casted list keeps its element type.
class G {
    private val edges: MutableMap<String, MutableList<String>> = mutableMapOf()

    fun add(from: String, to: String) {
        if (!edges.containsKey(from)) {
            edges.put(from, mutableListOf())
        }
        edges.get(from)!!.add(to)
    }

    fun size(v: String): Int {
        val list = edges.get(v)
        if (list == null) return 0
        return list.size
    }
}

fun main() {
    val g = G()
    g.add("A", "B")
    g.add("A", "C")
    g.add("A", "D")
    println("A: ${g.size("A")}")
    println("B: ${g.size("B")}")
}
