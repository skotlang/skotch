fun nameOrDefault(name: String?): String = name ?: "anonymous"

fun main() {
    println(nameOrDefault("Alice"))
    println(nameOrDefault(null))
}
