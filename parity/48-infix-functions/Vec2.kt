// `infix` member functions allow `a method b` call syntax. Pre-fix
// skotch's parser only recognized a hardcoded whitelist of stdlib
// infix functions (to, and, or, xor, shl, shr, ushr, contains, zip,
// until, downTo, step). User-defined `infix fun add(...)` wasn't
// recognized — calls like `a add b` failed with "unresolved
// identifier `add`".
//
// Fix at parser.rs:~3293: any Ident not followed by `(` / `{` / `else`
// is treated as a potential infix call. Loops left-associatively so
// chains like `a add b scale 2` parse as `(a add b) scale 2`.
// Critical guard: Newline OR Semi between lhs and the candidate Ident
// breaks the infix loop — without it, `val s = X(); s.setup(...)`
// would re-parse as `X().s(...).setup(...)`.

class Vec2(val x: Int, val y: Int) {
    infix fun add(other: Vec2): Vec2 = Vec2(x + other.x, y + other.y)
    infix fun scale(s: Int): Vec2 = Vec2(x * s, y * s)
    infix fun dot(other: Vec2): Int = x * other.x + y * other.y
    override fun toString(): String = "(" + x + ", " + y + ")"
}
