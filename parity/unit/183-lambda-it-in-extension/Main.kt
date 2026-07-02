// Isolates the "unresolved reference 'it'" error from
// parity/full/102-result — inside a lambda passed to a cross-file
// extension function, `it` should bind to the lambda's implicit
// single parameter. Skotch's resolver was reporting `it` as
// unresolved in the context `err.mapError { it.uppercase() }` where
// mapError is imported from another package.
import foo.Container
import foo.mapValue
import foo.foldValue

fun main() {
    val c = Container("hello")
    val upper = c.mapValue { it.uppercase() }
    println(upper.value)
    val rev = c.mapValue { it.reversed() }
    println(rev.value)
    val folded = c.foldValue(0) { acc, s -> acc + s.length }
    println(folded)
}
