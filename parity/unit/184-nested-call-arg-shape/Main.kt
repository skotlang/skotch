// Isolates the `Return(Call(Reference, Call, Reference))` bail from
// parity/full/102-result — kotlin-result's `tryFilter` body is
// literally `return tryFilterTo(ArrayList(), predicate)` where the
// first arg to the outer call is another Call. skotch's mir-lower
// body walker bails on this shape and drops the whole body. Kotlinc
// happily nests the calls.
fun combine(prefix: String, list: List<Int>): String =
    "$prefix:${list.joinToString(",")}"

fun forwardCombine(items: List<Int>): String {
    return combine(makePrefix(), items)
}

fun makePrefix(): String = "vals"

fun wrapCallAsRhs(x: Int, y: Int): Int {
    return foldSum(makeList(x), y)
}

fun makeList(n: Int): List<Int> = listOf(n, n + 1, n + 2)
fun foldSum(xs: List<Int>, delta: Int): Int {
    var s = delta
    for (v in xs) s += v
    return s
}

fun main() {
    println(forwardCombine(listOf(1, 2, 3)))
    println(wrapCallAsRhs(10, 100))
    println(forwardCombine(listOf()))
}
