// `class X private constructor(...)` — visibility modifier on the
// primary constructor between the class name (and type params) and
// the LParen. Pre-fix the parser at parser.rs:~785 went straight
// from `parse_type_params()` to checking `LParen` for ctor params;
// the `private` and `constructor` tokens trickled into the
// constructor-param parser as bogus tokens, eventually surfacing
// as `MIR: cross-file stub class has empty name`.
//
// Fix at parser.rs:~787 accepts and consumes optional
// `[private|protected|internal] constructor` between the type
// params and the LParen. Visibility is informational on JVM — the
// class's own ACC_PUBLIC stays — but the keyword sequence must
// parse so the rest of the class body is recognized.

class Color private constructor(val r: Int, val g: Int, val b: Int) {
    companion object {
        fun rgb(r: Int, g: Int, b: Int): Color = Color(r, g, b)
        fun gray(level: Int): Color = Color(level, level, level)
        fun white(): Color = Color(255, 255, 255)
        fun black(): Color = Color(0, 0, 0)
    }
    override fun toString(): String = "Color(" + r + ", " + g + ", " + b + ")"
}

fun main() {
    println(Color.white())
    println(Color.black())
    println(Color.gray(128))
    println(Color.rgb(255, 100, 50))
}
