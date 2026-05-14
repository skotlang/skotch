enum class V { A, B }

class Holder(val s: V)

fun pickIt(h: Holder, fn: (Holder) -> Int): Int = fn(h)

fun main() {
    val h = Holder(V.A)
    val x = pickIt(h) { it: Holder -> if (it.s == V.A) 1 else 2 }
    println(x)
}
