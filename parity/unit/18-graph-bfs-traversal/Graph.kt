// Generic adjacency-list graph with directed / undirected edges and a
// breadth-first traversal that returns the visit order.
//
// Sophistication step over example 17:
//   - one type parameter `V` used as both a Map key AND a List element
//     (exercises equals/hashCode plumbing through generic erasure)
//   - three different stdlib collection types interacting in one body:
//     MutableMap<V, MutableList<V>> backing store, MutableSet<V> for
//     visited tracking, MutableList<V> as a BFS queue
//   - null-assertion (`!!`) on Map.get since the addEdge path
//     guarantees both endpoints have an entry
//   - `removeAt(0)` to use a MutableList as a FIFO queue
class Graph<V>(val directed: Boolean) {
    private val edges: MutableMap<V, MutableList<V>> = mutableMapOf()

    fun addNode(v: V) {
        if (!edges.containsKey(v)) {
            edges.put(v, mutableListOf())
        }
    }

    fun addEdge(from: V, to: V) {
        addNode(from)
        addNode(to)
        edges.get(from)!!.add(to)
        if (!directed) {
            edges.get(to)!!.add(from)
        }
    }

    fun neighbors(v: V): List<V> {
        val list = edges.get(v)
        if (list == null) return mutableListOf()
        return list
    }

    fun bfs(start: V): List<V> {
        val visited: MutableSet<V> = mutableSetOf()
        val queue: MutableList<V> = mutableListOf()
        val order: MutableList<V> = mutableListOf()

        visited.add(start)
        queue.add(start)

        while (queue.isNotEmpty()) {
            val node = queue.removeAt(0)
            order.add(node)
            val ns = neighbors(node)
            var i = 0
            while (i < ns.size) {
                val next = ns[i]
                if (!visited.contains(next)) {
                    visited.add(next)
                    queue.add(next)
                }
                i++
            }
        }
        return order
    }
}
