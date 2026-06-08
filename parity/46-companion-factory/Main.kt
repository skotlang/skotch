// Drives the cross-file companion factories. Each `Class.factory()`
// dispatch goes through the synthesized companion-object static
// helper that kotlinc emits for `companion object { fun X(): T }`.

fun main() {
    // Color factories
    println(Color.white())                 // Color(255, 255, 255)
    println(Color.black())                 // Color(0, 0, 0)
    println(Color.gray(128))               // Color(128, 128, 128)
    println(Color.rgb(255, 100, 50))       // Color(255, 100, 50)
    println(Color.transparent())           // Color(0, 0, 0)
    println("---")

    // Shape factories
    println(Shape.triangle())              // Shape(triangle, 3 sides)
    println(Shape.square())                // Shape(square, 4 sides)
    println(Shape.pentagon())              // Shape(pentagon, 5 sides)
    println(Shape.ngon(8))                 // Shape(polygon, 8 sides)
    println(Shape.ngon(12))                // Shape(polygon, 12 sides)
    println("---")

    // Factories stored as vals — instance methods still work.
    val c1 = Color.rgb(10, 20, 30)
    val c2 = Color.gray(50)
    println(c1.r)                          // 10
    println(c1.g)                          // 20
    println(c1.b)                          // 30
    println(c2.r)                          // 50

    val s = Shape.ngon(7)
    println(s.name)                        // polygon
    println(s.sides)                       // 7
}
