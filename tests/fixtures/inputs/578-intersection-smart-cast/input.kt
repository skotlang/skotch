fun describe(value: Any): String {
    if (value is String && value.length > 3) {
        return "long string: $value"
    }
    if (value is Int) {
        return "integer: $value"
    }
    return "other"
}

fun main() {
    println(describe("hello"))
    println(describe("hi"))
    println(describe(42))
    println(describe(true))
}
