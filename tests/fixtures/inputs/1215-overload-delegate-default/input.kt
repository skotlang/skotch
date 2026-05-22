fun build(prefix: String): String = build(prefix, 0, false)
fun build(prefix: String, n: Int, flag: Boolean): String = "$prefix/$n/$flag"

fun main() {
    val s = build("hi")
    println(s)
}
