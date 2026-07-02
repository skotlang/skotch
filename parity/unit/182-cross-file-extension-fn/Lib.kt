package foo

class Container(val value: Int)

// A top-level extension function in another file/package, mirroring
// `Result<V,E>.getOr(default: V): V` in the kotlin-result library.
fun Container.doubled(): Int = value * 2

fun Container.plus(delta: Int): Int = value + delta

fun Container.describe(prefix: String): String = "$prefix:$value"
