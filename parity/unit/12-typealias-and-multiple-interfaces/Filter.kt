// `typealias` for a function type — equivalent to `(Int) -> Boolean`
// everywhere it's used. Useful for naming complex callback shapes.
typealias Predicate = (Int) -> Boolean

// Two unrelated interfaces. Concrete classes are free to implement
// both, exercising multiple-interface inheritance.
interface Matcher {
    fun matches(n: Int): Boolean
}

interface Counter {
    fun hits(): Int
    fun reset()
}

// One class, two interfaces. The Predicate alias resolves to a
// `(Int) -> Boolean` function type in the primary constructor.
class CountingMatcher(private val pred: Predicate) : Matcher, Counter {
    private var _hits: Int = 0

    override fun matches(n: Int): Boolean {
        val ok = pred(n)
        if (ok) {
            _hits += 1
        }
        return ok
    }

    override fun hits(): Int = _hits

    override fun reset() {
        _hits = 0
    }
}
