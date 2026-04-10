// TODO (PR #1.5): supports `$ident` interpolation. Requires a Concat
// intrinsic that the JVM backend lowers to StringBuilder.append calls.
fun main() {
    val name = "world"
    println("Hello, $name!")
}
