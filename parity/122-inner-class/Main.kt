class Outer2(val tag: String) {
    inner class Inner(val n: Int) {
        fun describe(): String = "${tag}:$n"
    }
    fun make(n: Int): Inner = Inner(n)
}

fun main() {
    val o = Outer2("hi")
    println(o.make(5).describe())
    println(o.make(99).describe())
}
