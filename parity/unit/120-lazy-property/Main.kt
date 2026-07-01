val computed: Int by lazy {
    println("computing")
    7 * 6
}

fun main() {
    println("before")
    println(computed)
    println(computed)
    println("after")
}
