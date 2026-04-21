class Secret {
    private val hidden: String = "secret"
    fun reveal(): String = hidden
}

fun main() {
    val s = Secret()
    println(s.reveal())
    // s.hidden would be a compile error in real Kotlin
}
