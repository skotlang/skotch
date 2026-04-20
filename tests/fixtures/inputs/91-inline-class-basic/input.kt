class Password(val value: String)

fun validate(pw: Password): Boolean = pw.value.length >= 8

fun main() {
    val pw = Password("secret123")
    println(validate(pw))
}
