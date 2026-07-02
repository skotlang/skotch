// Isolates the `Property(Reference)` bail from parity/full/102-result
// — kotlin-result's `tryFold` opens with `var accumulator = initial`
// where `initial` is a fn parameter (Reference). skotch's mir-lower
// body walker bails on this shape when it appears as the first
// statement of a fn body. Kotlinc emits a straight `iload_2; istore
// accumulator`.
fun accumulateInt(seed: Int, xs: List<Int>): Int {
    var acc = seed
    for (v in xs) acc += v
    return acc
}

fun accumulateStr(seed: String, xs: List<String>): String {
    var acc = seed
    for (s in xs) acc = "$acc-$s"
    return acc
}

fun <T> firstOr(list: List<T>, fallback: T): T {
    var chosen = fallback
    for (v in list) {
        chosen = v
        break
    }
    return chosen
}

fun main() {
    println(accumulateInt(10, listOf(1, 2, 3)))
    println(accumulateStr("root", listOf("a", "b")))
    println(firstOr(listOf(7, 8, 9), 0))
    println(firstOr(listOf<Int>(), -1))
}
