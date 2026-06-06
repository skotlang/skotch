// Regression: typeck must recognize CROSS-file class declarations
// when checking a declared annotation against a constructor-call
// initializer. Before the fix, `val e: Sealed = Sub(...)` (where
// Sealed and Sub are declared in a different file in the same
// package) reported "type mismatch: expected <class>, found Any"
// because typeck's `env.types` was only populated from the
// current file's decls. Now also seeds from `package_symbols`.
sealed class Shape {
    abstract fun area(): Double
}

class Circle(val r: Double) : Shape() {
    override fun area(): Double = 3.14 * r * r
}

class Square(val s: Double) : Shape() {
    override fun area(): Double = s * s
}

fun main() {
    val a: Shape = Circle(2.0)
    val b: Shape = Square(3.0)
    println("a area = ${a.area()}")
    println("b area = ${b.area()}")
}
