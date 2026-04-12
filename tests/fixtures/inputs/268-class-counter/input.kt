class Box(val value: Int) {
    fun doubled(): Int = value * 2
    fun description(): String = "Box"
}

fun main() {
    val b = Box(21)
    println(b.value)
    println(b.doubled())
    println(b.description())
}
