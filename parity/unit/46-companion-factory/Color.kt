// `Color` uses the factory-method pattern: the primary constructor
// is `private`, so callers must go through `Color.rgb(...)`,
// `Color.gray(...)`, etc. Probes:
//   - `class X private constructor(...)` — visibility on primary
//     ctor (parsed but not enforced at compile time — skotch ignores
//     the visibility, but the keyword sequence must parse)
//   - companion object with multiple factory methods
//   - companion methods returning the enclosing class
//   - implicit `Color(...)` call inside companion (resolves to the
//     class's primary ctor, NOT the recursive companion factory)

class Color private constructor(val r: Int, val g: Int, val b: Int) {
    companion object {
        fun rgb(r: Int, g: Int, b: Int): Color = Color(r, g, b)
        fun gray(level: Int): Color = Color(level, level, level)
        fun white(): Color = Color(255, 255, 255)
        fun black(): Color = Color(0, 0, 0)
        fun transparent(): Color = Color(0, 0, 0)  // 0 alpha (not modeled)
    }
    override fun toString(): String = "Color(" + r + ", " + g + ", " + b + ")"
}
