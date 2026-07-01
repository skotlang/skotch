class Box2(val value: Int) {
    val doubled: Int get() = value * 2
    val info: String get() = "Box($value, doubled=$doubled)"
    val isPositive: Boolean get() = value > 0
}

fun main() {
    val b = Box2(7)
    println(b.value)
    println(b.doubled)
    println(b.info)
    println(b.isPositive)
    println(Box2(-3).isPositive)
}
