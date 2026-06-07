fun joinNodes(xs: List<String>): String {
    val sb = StringBuilder()
    var i = 0
    while (i < xs.size) {
        if (i > 0) sb.append(", ")
        sb.append(xs[i])
        i++
    }
    return sb.toString()
}

fun main() {
    // Small undirected graph:
    //   A — B — D
    //   |       |
    //   C ————— E
    val g = Graph<String>(false)
    g.addEdge("A", "B")
    g.addEdge("A", "C")
    g.addEdge("B", "D")
    g.addEdge("C", "E")
    g.addEdge("D", "E")

    val orderFromA = g.bfs("A")
    println("BFS from A: ${joinNodes(orderFromA)}")

    val orderFromE = g.bfs("E")
    println("BFS from E: ${joinNodes(orderFromE)}")

    // Directed version of the same shape — note D doesn't reach back to B.
    val dg = Graph<String>(true)
    dg.addEdge("A", "B")
    dg.addEdge("A", "C")
    dg.addEdge("B", "D")
    dg.addEdge("C", "E")
    dg.addEdge("D", "E")

    println("directed BFS from A: ${joinNodes(dg.bfs("A"))}")
    println("directed BFS from D: ${joinNodes(dg.bfs("D"))}")
}
