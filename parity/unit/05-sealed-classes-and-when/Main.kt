fun main() {
    val shapes: List<Shape> = listOf(
        Circle(2.0),
        Rectangle(3.0, 4.0),
        Triangle(5.0, 6.0),
    )

    for (s in shapes) {
        println("${describe(s)} -> area=${s.area()}")
    }

    val totalArea = shapes.fold(0.0) { acc, s -> acc + s.area() }
    println("total area = $totalArea")
}
