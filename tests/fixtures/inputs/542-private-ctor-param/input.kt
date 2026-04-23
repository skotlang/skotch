class Secret(private val code: Int) {
    fun reveal(): Int = code
    fun check(guess: Int): Boolean = guess == code
}

fun main() {
    val s = Secret(42)
    println(s.reveal())
    println(s.check(42))
    println(s.check(99))
}
