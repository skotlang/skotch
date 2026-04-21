fun describe(x: Any): String {
    if (x is String && x.length > 3) {
        return "long string: ${x.uppercase()}"
    }
    return "other"
}

fun main() {
    println(describe("hello"))
    println(describe("hi"))
    println(describe(42))
}
