fun <T, R> T.run(block: T.() -> R): R = block()
fun maybe(p: () -> Int): Int = p()
fun main() {
    println(maybe { 42 })
    println(7.run { this + 3 })
}
