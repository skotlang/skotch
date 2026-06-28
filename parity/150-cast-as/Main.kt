open class Shape2
class Square2(val side: Int) : Shape2()

fun area(s: Shape2): Int {
    val sq = s as Square2
    return sq.side * sq.side
}

fun main() {
    println(area(Square2(5)))
    println(area(Square2(7)))
    try {
        area(Shape2())
    } catch (e: ClassCastException) {
        println("caught:cast")
    }
}
