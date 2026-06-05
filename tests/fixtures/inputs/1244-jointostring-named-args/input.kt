// Regression: joinToString must route named args to the correct slot
// of `CollectionsKt.joinToString$default`.
//
// Previously the lowering hardcoded `all_args[1]` as the separator and
// ignored the AST-level arg names, so
// `joinToString(prefix = "[", postfix = "]", separator = ", ")`
// produced output like `6[7[8[9[10` — the prefix value ended up in the
// separator slot and the other two named args were dropped.
fun main() {
    val nums = listOf(6, 7, 8, 9, 10)

    // All three named (out-of-declaration-order on purpose):
    println(nums.joinToString(prefix = "[", postfix = "]", separator = ", "))

    // Two named, no positional:
    println(nums.joinToString(prefix = "<", postfix = ">"))

    // Single positional separator (the pre-existing simple case):
    println(nums.joinToString(" | "))
}
