fun main() {
    val sb = StringBuilder()
    sb.append("hello")
    sb.append(' ')
    sb.append("world")
    sb.append(' ')
    sb.append(42)
    println(sb.toString())
    println(sb.length)

    val sb2 = StringBuilder("init")
    sb2.append("-ext")
    println(sb2)
}
