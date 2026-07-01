fun String.shout(): String = this.uppercase() + "!"
fun Int.times(s: String): String {
    val sb = StringBuilder()
    for (i in 0 until this) sb.append(s)
    return sb.toString()
}

fun main() {
    println("hello".shout())
    println(3.times("ab"))
    println(0.times("x"))
}
