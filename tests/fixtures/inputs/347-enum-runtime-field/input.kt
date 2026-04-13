enum class Coin(val cents: Int) {
    PENNY(1),
    NICKEL(5),
    DIME(10),
    QUARTER(25)
}

fun value(c: Coin): Int = c.cents

fun main() {
    println(value(Coin.PENNY))
    println(value(Coin.QUARTER))
    println(Coin.DIME.cents)
}
