// Regression: a `when (subj) { is A -> ...; is B -> { val n = ...; ... };
// is C -> ... }` branch whose body introduces its OWN locals (`val n`)
// must not strand the smart-cast scope entry behind those locals.
// Previously the per-branch pop only removed `smart_cast_entries`
// trailing scope entries — so the body's own `val n` was popped
// first, leaving the smart-cast `(subj, NarrowedLocal)` in scope.
// The NEXT branch's smart-cast then read that stale local as its
// "old" value, producing a use-before-def → the whole function was
// silently rewritten to a `aconst_null; areturn` stub and every call
// site got an NPE.
sealed interface Shape {
    fun area(): Int
}

class Square(val side: Int) : Shape {
    override fun area(): Int = side * side
}

class Rect(val w: Int, val h: Int) : Shape {
    override fun area(): Int = w * h
}

class Empty : Shape {
    override fun area(): Int = 0
}

fun grow(s: Shape): Shape = when (s) {
    is Square -> Square(s.side + 1)
    is Rect -> {
        // local `n` declared INSIDE the branch — its scope entry
        // sits after the smart-cast, so a naive pop would lose the
        // smart-cast and pollute the next branch.
        val n = s.w + s.h
        Rect(s.w + 1, n)
    }
    is Empty -> Empty()
}

fun main() {
    var s: Shape = Square(2)
    s = grow(s); println(s.area())
    s = Rect(3, 4)
    s = grow(s); println(s.area())
    s = Empty()
    s = grow(s); println(s.area())
}
