package test

class Dp(val value: Float)

interface Density {
    fun Dp.roundToPx(): Int = (value * 2).toInt()
}

class MyDensity : Density

fun Density.measureIt(dp: Dp): Int = dp.roundToPx()

fun main() {
    val scope = MyDensity()
    println(scope.measureIt(Dp(10.0f)))
}
