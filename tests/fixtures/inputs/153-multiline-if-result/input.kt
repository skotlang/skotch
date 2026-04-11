fun main() {
    val x = 42
    val result = if (x > 0) {
        val doubled = x * 2
        println("Doubled: $doubled")
        doubled
    } else {
        0
    }
    println("Result: $result")
}
