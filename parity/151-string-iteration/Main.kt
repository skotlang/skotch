fun main() {
    val s = "kotlin"
    for (c in s) print("$c ")
    println()
    for ((i, c) in s.withIndex()) print("$i:$c ")
    println()
    println(s.reversed())
    println(s.toCharArray().size)
    println(s.count { it == 't' || it == 'i' })
}
