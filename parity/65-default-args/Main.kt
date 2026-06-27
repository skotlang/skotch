fun greet(name: String = "world", count: Int = 1): String {
    val parts = mutableListOf<String>()
    for (i in 0 until count) parts.add(name)
    return parts.joinToString(" ")
}

fun main() {
    println(greet())
    println(greet("hi"))
    println(greet(count = 3))
    println(greet("yo", 2))
}
