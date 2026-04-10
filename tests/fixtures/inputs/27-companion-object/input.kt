// TODO: companion objects (lowered to a static $Companion inner class).
class Counter {
    companion object {
        fun zero(): Int = 0
    }
}

fun main() {
    println(Counter.zero())
}
