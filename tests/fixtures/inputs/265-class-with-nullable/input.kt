fun nameOrDefault(name: String?): String = name ?: "World"

fun main() {
    println(nameOrDefault("Kotlin"))
    println(nameOrDefault(null))
}
