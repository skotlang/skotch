class Builder(val name: String) {
    infix fun with(suffix: String): String = name + suffix
}

fun main() {
    val b = Builder("Hello")
    println(b.with(" World"))
}
