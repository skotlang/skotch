package test

class Dp(val value: Float)

interface Density {
    fun Dp.roundToPx(): Int = (value * 2).toInt()
}

class Helper {
    fun runIt(block: () -> Int): Int = block()
}

class MyDensity : Density {
    fun measureIt(dp: Dp): Int {
        val helper = Helper()
        return helper.runIt {
            dp.roundToPx() + 1
        }
    }
}

fun main() {
    val d = MyDensity()
    println(d.measureIt(Dp(10.0f)))
}
