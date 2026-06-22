fun Int.cents(): Money = Money(this.toLong())

data class Money(val cents: Long)

fun test() {
    val x = 30.cents()
}
