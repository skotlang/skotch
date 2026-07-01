// A small money type that demonstrates Kotlin operator overloading
// (`+`, `*`), data-class equality, and value-type-style usage.
// `cents` is a Long so arithmetic stays integer-precise.
data class Money(val cents: Long) {

    operator fun plus(other: Money): Money = Money(cents + other.cents)

    operator fun times(n: Int): Money = Money(cents * n)

    fun format(): String {
        val dollars = cents / 100
        val rem = cents % 100
        val remStr = if (rem < 10) "0$rem" else "$rem"
        return "$$dollars.$remStr"
    }
}

// Extension functions on Int — the standard Kotlin idiom for
// constructor-like helpers (`5.dollars()`, `99.cents()`).
fun Int.dollars(): Money = Money(this.toLong() * 100L)

fun Int.cents(): Money = Money(this.toLong())
