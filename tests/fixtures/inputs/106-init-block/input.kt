class Validated(val value: Int) {
    init {
        require(value > 0)
    }
}

fun main() {
    val v = Validated(42)
    println(v.value)
}
