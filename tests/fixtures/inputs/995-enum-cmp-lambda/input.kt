enum class V { A, B }

fun pick(fn: (V) -> Int): Int = fn(V.A)

fun main() {
    val x = pick { if (it == V.A) 1 else 2 }
    println(x)
}
