package test

class Dp(val value: Float) {
    operator fun div(divisor: Float): Dp = Dp(value / divisor)
    operator fun minus(other: Float): Dp = Dp(value - other)
}

fun halveDp(x: Dp): Dp = x / 2f

fun shrinkDp(x: Dp, amount: Float): Dp = x - amount

fun main() {
    println(halveDp(Dp(10.0f)).value)
    println(shrinkDp(Dp(10.0f), 3.0f).value)
}
