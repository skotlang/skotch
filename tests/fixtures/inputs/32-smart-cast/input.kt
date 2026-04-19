fun describe(x: Any) {
    if (x is String) {
        println(x.length)
    }
}

fun main() {
    describe("hello")
    describe(42)
}
