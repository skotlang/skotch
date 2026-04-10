inline fun <reified T> isInstance(value: Any): Boolean = value is T

fun main() {
    println(isInstance<String>("hello"))
    println(isInstance<Int>("hello"))
    println(isInstance<Int>(42))
}
