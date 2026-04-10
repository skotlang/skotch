// TODO: nullable types and the elvis operator `?:`.
fun greet(name: String?) {
    val n = name ?: "stranger"
    println(n)
}

fun main() {
    greet(null)
}
