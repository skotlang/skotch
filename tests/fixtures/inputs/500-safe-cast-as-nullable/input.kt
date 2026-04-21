fun safeCast(x: Any): String? = x as? String

fun main() {
    println(safeCast("hello"))
    println(safeCast(42))
}
