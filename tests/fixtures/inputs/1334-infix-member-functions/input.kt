// User-defined `infix fun` member functions called with infix
// syntax `a method b`. Pre-fix parser only recognized a hardcoded
// whitelist of stdlib infix keywords (to/and/or/xor/shl/shr/ushr/
// contains/zip/until/downTo/step). User `infix fun add` failed.
//
// Fix at parser.rs:~3293: treat any Ident not followed by `(`/`{`/
// `else` as a potential infix call, then loop left-associatively
// for chains like `a add b scale 2` → `(a add b) scale 2`.
//
// Critical guard: walk back from current pos before infix-detection.
// If a Newline or Semi token sits between lhs and the current pos,
// lhs's statement ended; the Ident here starts the NEXT statement
// (not an infix continuation of lhs). Without that guard `val s =
// Service(); s.setup(...)` re-parsed as `Service().s(...).setup(...)`
// — broke ~50 existing fixtures before the guard landed.

class Vec2(val x: Int, val y: Int) {
    infix fun add(other: Vec2): Vec2 = Vec2(x + other.x, y + other.y)
    infix fun scale(s: Int): Vec2 = Vec2(x * s, y * s)
    override fun toString(): String = "(" + x + ", " + y + ")"
}

fun main() {
    val a = Vec2(1, 2)
    val b = Vec2(3, 4)
    println(a add b)
    println(a scale 3)
    println(a add b scale 2)
}
