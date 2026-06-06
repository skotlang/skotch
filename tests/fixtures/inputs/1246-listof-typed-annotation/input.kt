// Regression: `val xs: List<Shape> = listOf(SubA(...), SubB(...))` must
// typecheck — the `listOf` call returns `Ty::Class("java/util/List")`
// which is assignable to the declared `List<Shape>` (also erased to
// `java/util/List`). Before the fix, the listOf return fell through to
// `Ty::Any` and the assignment failed with
// "type mismatch: expected <class>, found Any".
sealed class Shape

class Circle(val radius: Double) : Shape()

class Square(val side: Double) : Shape()

fun main() {
    val shapes: List<Shape> = listOf(Circle(1.0), Square(2.0))
    println(shapes.size)
}
