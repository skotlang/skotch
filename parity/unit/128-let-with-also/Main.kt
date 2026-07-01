fun main() {
    val s = "hello"
    val len = s.let { it.length }
    println(len)
    val u = s.let { it.uppercase() }
    println(u)
    val n: Int? = 42
    val r = n?.let { it * 2 } ?: -1
    println(r)
}
