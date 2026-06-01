// @JvmField on a property suppresses the synthesized getX accessor
// and exposes the field as a public Java-interop member directly.
// Two properties on Point: x is @JvmField (no getter), y is plain (gets a
// `public int getY()` accessor).

class Point(@JvmField val x: Int, val y: Int)

fun main() {
    val p = Point(3, 5)
    println(p.x)
    println(p.y)
}
