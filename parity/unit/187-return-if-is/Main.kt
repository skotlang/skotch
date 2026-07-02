// Isolates the `Return(If(Is, Reference, Reference))` bail from
// parity/full/102-result — several library helpers close with
// `return if (v is X) v else default`. skotch's mir-lower body
// walker bails on the `if (v is X) a else b` shape when it appears
// directly under `return`.
fun asIntOrZero(x: Any): Int {
    return if (x is Int) x else 0
}

fun asStringOrEmpty(x: Any): String {
    return if (x is String) x else ""
}

fun coerceOrDefault(x: Any, fallback: Int): Int {
    return if (x is Int) x else fallback
}

fun main() {
    println(asIntOrZero(42))
    println(asIntOrZero("nope"))
    println(asStringOrEmpty("hi"))
    println(asStringOrEmpty(7))
    println(coerceOrDefault(3, -1))
    println(coerceOrDefault(3.14, -1))
}
