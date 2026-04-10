class Square {
    val side: Int
    constructor(s: Int) { side = s }
    constructor() : this(1)
    fun area(): Int = side * side
}

fun main() {
    println(Square(5).area())
    println(Square().area())
}
