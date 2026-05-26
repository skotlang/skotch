// A top-level factory function may share its name with a class (Kotlin allows
// it — e.g. Compose's `RoundedCornerShape(Dp,…)` factory next to the
// `RoundedCornerShape(CornerSize,…)` constructor). When the call's args fit the
// factory but not the constructor, resolve to the factory rather than emitting
// a type-mismatched `<init>` call (rejected by the verifier / d8).
class Box(val s: String)

fun Box(n: Int): Box = Box(n.toString())

val b = Box(42)

fun main() {
    println(b.s)
}
