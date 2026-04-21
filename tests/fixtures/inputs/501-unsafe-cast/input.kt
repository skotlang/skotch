fun unsafeCast(x: Any): String = x as String

fun main() {
    println(unsafeCast("hello"))
    try {
        unsafeCast(42)
    } catch (e: ClassCastException) {
        println("caught ClassCastException")
    }
}
