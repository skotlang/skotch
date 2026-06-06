// Regression: `object Foo : Iface` — the parser must accept the
// supertype clause on an `object`, the resolver must surface the
// `implements` edge cross-file, and mir-lower must emit the
// MirClass with `interfaces=[Iface]` so the JVM backend lists the
// interface in the class file. `var x: Iface = Foo` then type-checks
// and `Foo` lowers to `getstatic Foo.INSTANCE` (not a null placeholder).
sealed interface Shape {
    fun describe(): String
}

object Empty : Shape {
    override fun describe(): String = "empty"
}

class Disk(val radius: Int) : Shape {
    override fun describe(): String = "disk($radius)"
}

fun main() {
    var s: Shape = Empty
    println(s.describe())
    s = Disk(7)
    println(s.describe())
}
