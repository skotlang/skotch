fun greet(name: String?): String {
    if (name != null) {
        return "Hello, " + name.uppercase()
    }
    return "Hello, stranger"
}

fun main() {
    println(greet("world"))
    println(greet(null))
}
