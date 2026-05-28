// User-declared `inline fun` whose body is a single expression
// referencing its parameter. With the inline-fn body substitution
// pass active, the call `square(7)` lowers without a real static
// method call — the body `x * x` is spliced in at the call site.

inline fun square(x: Int): Int = x * x

fun main() {
    println(square(7))
}
