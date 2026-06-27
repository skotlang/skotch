fun <T> init(b: () -> T): T = b()

val cached: Int = init { 42 + 1 }

fun main() {
    println(cached)
    println(cached)
}
