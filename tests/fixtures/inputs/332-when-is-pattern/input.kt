fun describe(obj: Any): String = when {
    obj is String -> "string"
    obj is Int -> "int"
    obj is Boolean -> "bool"
    else -> "other"
}

fun main() {
    println(describe("hello"))
    println(describe(42))
    println(describe(true))
}
