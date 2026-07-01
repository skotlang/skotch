class Rect(val w: Int, val h: Int) {
    val area: Int
        get() = w * h
    val isSquare: Boolean
        get() = w == h
}

fun main() {
    val r = Rect(3, 4)
    val s = Rect(5, 5)
    println(r.area)
    println(r.isSquare)
    println(s.area)
    println(s.isSquare)
}
