fun main() {
    val a = Vec2(1, 2)
    val b = Vec2(3, 4)

    // Single infix
    println(a add b)               // (4, 6)
    println(a scale 3)             // (3, 6)
    println(a dot b)               // 11

    // Chained infix — left-associative
    println(a add b scale 2)       // (8, 12)
    println(a add b add b)         // (7, 10)

    // Mixed with normal method-call syntax
    val c = a.add(b).scale(2)
    println(c)                     // (8, 12)
}
