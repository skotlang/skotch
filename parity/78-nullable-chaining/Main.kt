class Inner(val v: Int)
class Outer(val inner: Inner?)

fun depth(o: Outer?): Int = o?.inner?.v ?: -1

fun main() {
    println(depth(Outer(Inner(7))))
    println(depth(Outer(null)))
    println(depth(null))
}
