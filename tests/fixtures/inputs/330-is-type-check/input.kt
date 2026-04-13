fun typeOf(x: Any): String = when {
    x is String -> "String"
    x is Int -> "Int"
    x is Boolean -> "Boolean"
    else -> "Other"
}

fun main() {
    println(typeOf("hello"))
    println(typeOf(42))
    println(typeOf(true))
}
