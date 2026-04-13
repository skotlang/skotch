fun describe(x: Any): String {
    if (x is String) {
        return "length=" + x.length
    }
    return "not string"
}

fun main() {
    println(describe("hello"))
    println(describe(42))
}
