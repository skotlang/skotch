import java.util.HashMap

fun main() {
    val map = HashMap<String, Int>()
    map.put("a", 1)
    map.put("b", 2)
    map.put("c", 3)
    println(map.size)
    println(map.get("b"))
}
