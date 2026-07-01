fun typeOf(x: Any): String = when {
    x is Int -> "int:$x"
    x is String -> "str:$x"
    x is Boolean -> "bool:$x"
    x is List<*> -> "list:size=${x.size}"
    else -> "other"
}

fun main() {
    println(typeOf(42))
    println(typeOf("hi"))
    println(typeOf(true))
    println(typeOf(listOf(1, 2, 3)))
    println(typeOf(3.14))
}
