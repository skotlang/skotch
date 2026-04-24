value class Password(val value: String)

fun main() {
    val pw = Password("secret123")
    println(pw)
    println(pw.value)
}
