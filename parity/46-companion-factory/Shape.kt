// Second class with the same pattern — confirms multi-class
// companion-factory works (each class's `Color`/`Shape` companion
// is independently dispatched, no cross-class confusion).

class Shape private constructor(val name: String, val sides: Int) {
    companion object {
        fun triangle(): Shape = Shape("triangle", 3)
        fun square(): Shape = Shape("square", 4)
        fun pentagon(): Shape = Shape("pentagon", 5)
        fun ngon(n: Int): Shape = Shape("polygon", n)
    }
    override fun toString(): String = "Shape(" + name + ", " + sides + " sides)"
}
