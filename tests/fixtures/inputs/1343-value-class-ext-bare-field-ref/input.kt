// Phase LL regression: bare `m` inside the body of an inline
// extension on a `@JvmInline value class V(val m: IntArray)` must
// resolve to the value-class underlying val (rewritten by the
// post-pass to slot 0). Pre-fix the mini-walker / lower_rich →
// lower_inline fallback path inside `method_simple_body_full` ran
// without a CLASS_METHOD_CTX, so `class_field_lookup("m")` returned
// None and `m.fill(0)` bailed kind=DotQualified → whole body dropped
// to a stub. KotlinCrypto/hash's BLAKE2 family hits this through
// `Bit32Message.fill()` / `Bit32Message.get(index)` inside the
// compression loop; the cascade ended up zero'ing the message bytes.

@JvmInline
internal value class Bit32Message(internal val m: IntArray)

internal inline fun Bit32Message.fill() {
    m.fill(0)
}

fun main() {
    val x = Bit32Message(IntArray(4) { it + 1 })
    x.fill()
    println(x.m.contentToString())
}
