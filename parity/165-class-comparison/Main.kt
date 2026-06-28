class Version(val major: Int, val minor: Int) : Comparable<Version> {
    override fun compareTo(other: Version): Int =
        if (major != other.major) major - other.major else minor - other.minor
    override fun toString(): String = "v$major.$minor"
}

fun main() {
    val a = Version(1, 5)
    val b = Version(1, 10)
    val c = Version(2, 0)
    println(a < b)
    println(b < c)
    println(c > a)
    println(a == Version(1, 5))
    val xs = listOf(c, a, b).sorted()
    println(xs)
}
