import java.util.LinkedList
import java.util.TreeMap

fun main() {
    val list = LinkedList<String>()
    list.add("hello")
    list.add("world")
    println(list.size)
    println(list.getFirst())

    val map = TreeMap<String, Int>()
    map.put("b", 2)
    map.put("a", 1)
    println(map.size)
    println(map.firstKey())
}
