// Regression for audit finding #1: ExternalClassKind::SealedClass was
// never constructed by the resolver, so cross-file `when (s: Sealed)`
// exhaustiveness checks didn't fire — the variant was dead. Fix:
// resolver branches on `c.is_sealed` first when computing the kind.
//
// This file's `when` over a sealed-class subject must accept all
// declared subclasses and the typeck reports no error. (The
// exhaustiveness check warns when a sealed `when` is missing branches;
// here we cover every subclass, so no warning either.)
sealed class Shape {
    abstract fun area(): Int
}

class Circle(val r: Int) : Shape() {
    override fun area(): Int = r * r * 3
}

class Square(val side: Int) : Shape() {
    override fun area(): Int = side * side
}

fun describe(s: Shape): String = when (s) {
    is Circle -> "circle of area ${s.area()}"
    is Square -> "square of area ${s.area()}"
}

fun main() {
    println(describe(Circle(3)))
    println(describe(Square(4)))
}
