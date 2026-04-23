fun main() {
    val home = System.getenv("HOME")
    println(home != null)
    val t = System.currentTimeMillis()
    println(t > 0)
    println(System.lineSeparator().length > 0)
}
