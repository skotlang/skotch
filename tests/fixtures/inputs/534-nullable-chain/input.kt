fun greet(name: String?): String {
    return name?.uppercase() ?: "STRANGER"
}

fun main() {
    println(greet("world"))
    println(greet(null))
}
