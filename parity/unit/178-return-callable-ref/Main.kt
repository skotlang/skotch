// Isolates the `Return(Call(Reference, CallableRef))` bail from
// parity/full/102-result — several library helpers close their body
// with `return items.foldSomething(::helper)`, where `::helper` is a
// top-level callable reference used as a function-typed argument.
// skotch's mir-lower typed body walker bails on the CallableRef arg
// shape and drops the whole body.
fun isPositive(n: Int): Boolean = n > 0
fun isEven(n: Int): Boolean = n % 2 == 0

fun countPositives(xs: List<Int>): Int {
    return xs.count(::isPositive)
}

fun countEvens(xs: List<Int>): Int {
    return xs.count(::isEven)
}

fun firstPositive(xs: List<Int>): Int? {
    return xs.firstOrNull(::isPositive)
}

fun main() {
    val values = listOf(-3, 0, 5, 8, -1, 12)
    println(countPositives(values))
    println(countEvens(values))
    println(firstPositive(values))
    println(firstPositive(listOf(-1, -2, -3)))
}
