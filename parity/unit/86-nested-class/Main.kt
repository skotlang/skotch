class Outer(val name: String) {
    class Nested(val tag: Int) {
        fun describe(): String = "nested:$tag"
    }
    fun makeNested(t: Int): Nested = Nested(t)
}

fun main() {
    val o = Outer("hi")
    val n = o.makeNested(5)
    println(n.describe())
    val n2 = Outer.Nested(42)
    println(n2.describe())
}
