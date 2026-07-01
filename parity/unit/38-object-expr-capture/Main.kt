// Two patterns:
//   1. Object expressions returned from factory fns (Producers.kt
//      + Factory.kt) — each capture lives on the synthesized class
//      as a field, threaded through the constructor.
//
//   2. Object expressions captured directly in main() — same
//      mechanism but exercises the in-scope-at-instantiation path.
//
// The simple-capture case (val tag captured by a Producer) and the
// multi-capture case (two String captures by a Producer) both
// exercise the per-method GetField prelude that lets body
// identifiers resolve.

fun main() {
    // ── Pattern 1: returned from factory ──────────────────────────
    val greeting = makeProducer("hello, ", "world")
    println(greeting.produce())                  // "hello, world"

    val warning = makeProducer(">> ", "watch out!")
    println(warning.produce())                   // ">> watch out!"

    val triple = makeScaler(3)
    println(triple.apply(2, 5))                  // 21

    val negate = makeScaler(-1)
    println(negate.apply(7, 3))                  // -10

    // ── Pattern 2: instantiated inline + captured in main ─────────
    val tag = "[main]"
    val inline = object : Producer {
        override fun produce(): String = tag + " inline"
    }
    println(inline.produce())                    // "[main] inline"

    val left = "L:"
    val right = ":R"
    val wrap = object : Producer {
        override fun produce(): String = left + "wrapped" + right
    }
    println(wrap.produce())                      // "L:wrapped:R"

    // ── BinaryOp inline capture ───────────────────────────────────
    val k = 100
    val biased = object : BinaryOp {
        override fun apply(a: Int, b: Int): Int = a * b + k
    }
    println(biased.apply(3, 4))                  // 112
}
