// `heapSum` walks an IntArray as a virtual binary heap: index 0
// is the root, children of i are 2*i+1 and 2*i+2. The helper
// captures `heap` and `size` from the outer scope (both val) and
// threads the running total through an explicit `acc` param —
// NOT through a captured `var total`. var-capture needs the
// Kotlin Ref.IntRef boxing pattern, which skotch doesn't yet
// emit (documented in v0.50 milestones as `var capture Ref
// boxing` gap). The accumulator-thread workaround keeps the
// example parity-clean while still exercising the local-fn
// shape with 2 captures + 2 explicit params + tree-recursive
// fan-out.

fun heapSum(heap: IntArray, size: Int): Int {
    fun visit(i: Int, acc: Int): Int {
        if (i >= size) return acc
        val withSelf = acc + heap[i]
        val withLeft = visit(2 * i + 1, withSelf)
        return visit(2 * i + 2, withLeft)
    }
    return visit(0, 0)
}
