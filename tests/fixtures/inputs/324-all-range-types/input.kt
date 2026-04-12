fun main() {
    var a = 0
    for (i in 1..5) { a += i }
    println(a)
    var b = 0
    for (i in 1 until 6) { b += i }
    println(b)
    var c = 0
    for (i in 5 downTo 1) { c += i }
    println(c)
}
