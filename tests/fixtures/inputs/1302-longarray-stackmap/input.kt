// StackMapTable verification_type for LongArray / DoubleArray /
// BooleanArray / ByteArray locals.
//
// Pre-fix: `write_slot_verif` only handled `Ty::IntArray` as an
// Object_variable_info; LongArray/DoubleArray/BooleanArray/ByteArray
// fell through the match's `_ =>` arm and emitted Integer (frame
// kind = 1). The verifier then rejected branch targets where the
// frame disagreed with the current stack (`Type '[J' is not
// assignable to integer (stack map, locals[N])`).
//
// Fix: extend `write_slot_verif` and `write_slot_verif_with_code`
// to map each typed array to its descriptor class
// (`[J`, `[D`, `[Z`, `[B`).

fun sumLongs(longs: LongArray): Long {
    var sum: Long = 0L
    var i = 0
    while (i < longs.size) {
        sum = sum + longs[i]
        i++
    }
    return sum
}

fun countTrue(bools: BooleanArray): Int {
    var n = 0
    var i = 0
    while (i < bools.size) {
        if (bools[i]) { n++ }
        i++
    }
    return n
}

fun main() {
    println(sumLongs(longArrayOf(1L, 2L, 3L)))
    println(countTrue(booleanArrayOf(true, false, true, true)))
}
