fun main() {
    val m = mutableMapOf<String, Int>()
    m["a"] = 1
    m["b"] = 2
    m["c"] = 3
    println(m.size)
    println(m["b"])
    m.remove("a")
    println(m.size)
    println(m.containsKey("b"))
    println(m.containsKey("a"))
    for ((k, v) in m) println("$k=$v")
}
