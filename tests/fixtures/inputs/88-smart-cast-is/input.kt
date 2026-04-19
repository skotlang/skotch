fun describe(obj: Any): String = when (obj) {
    is Int -> "integer"
    is String -> "string of length ${obj.length}"
    is Boolean -> "boolean"
    else -> "unknown"
}

fun main() {
    println(describe(42))
    println(describe("hello"))
    println(describe(true))
}
