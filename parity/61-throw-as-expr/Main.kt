fun pick(b: Boolean): Int {
    val r = if (b) 7 else throw IllegalStateException("no")
    return r
}

fun main() {
    println(pick(true))
    try {
        pick(false)
    } catch (e: IllegalStateException) {
        println("caught")
    }
}
