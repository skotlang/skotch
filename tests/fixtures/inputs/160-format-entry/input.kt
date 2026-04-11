fun formatEntry(name: String, age: Int, active: Boolean): String {
    return "$name (age $age) - ${if (active) "active" else "inactive"}"
}

fun main() {
    println(formatEntry("Alice", 30, true))
    println(formatEntry("Bob", 25, false))
}
