fun main() {
    val csv = "a,b,c,d,e"
    val parts = csv.split(",")
    println(parts.size)
    for (p in parts) println(p)
    val joined = parts.joinToString("|")
    println(joined)
    val first3 = parts.take(3).joinToString(",")
    println(first3)
}
