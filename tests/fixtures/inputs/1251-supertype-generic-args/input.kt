// Regression: the parser must accept generic argument lists on a
// class's supertype (`: Comparable<Self>` etc.). Before the fix, the
// parser saw `<` after the interface name as an unexpected token and
// rejected the whole declaration. We don't yet model supertype type
// args in the class declaration; the parser just consumes the
// `<...>` so the rest of the source parses.
class Box(val n: Int) : Comparable<Box> {
    override fun compareTo(other: Box): Int = 0
}

fun main() {
    val a = Box(1)
    val b = Box(2)
    println(a.compareTo(b))
}
