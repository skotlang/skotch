// Regression: `recv.method(args)` must not dispatch to a local var
// named `method` when that local is non-function-typed.
//
// Previously, when a local `val sum = ...` (an Int) was in scope, a
// subsequent `nums.sum()` call resolved to `sum.invoke(nums)`, producing
// `NoSuchMethodError 'java.lang.Object.invoke(java.lang.Object)'` at
// runtime. The local-as-callable fast path now only fires when the
// local's actual type is a function/lambda interface.
fun main() {
    val nums = listOf(1, 2, 3, 4, 5)

    // Shadow the stdlib `sum` extension with a same-named local Int.
    val sum = nums.fold(0) { acc, n -> acc + n }
    println("fold sum = $sum")

    // This must dispatch to `CollectionsKt.sumOfInt(nums)`, not to
    // `sum.invoke(nums)` (which would crash).
    val total = nums.sum()
    println("total = $total")
}
