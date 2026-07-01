fun classify(s: String): String {
    return try {
        s.toInt().toString()
    } catch (e: NumberFormatException) {
        "num-err"
    } catch (e: IllegalStateException) {
        "state-err"
    }
}

fun main() {
    println(classify("5"))
    println(classify("nope"))
}
