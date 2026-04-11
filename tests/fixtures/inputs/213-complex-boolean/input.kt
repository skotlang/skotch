fun main() {
    val a = 5
    val b = 10
    val c = 15
    println(a < b && b < c)
    println(a > b || c > b)
    println(!(a == b) && (c > a))
    println(a < b && b < c && c > 0)
}
