fun outer(): Int {
    var sum = 0
    fun add(x: Int) { sum += x }
    add(3); add(4); add(5)
    return sum
}

fun main() {
    println(outer())
}
