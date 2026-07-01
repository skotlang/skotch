// 3-way merge of sorted IntArrays using the MinHeap.
//
// Encoding trick to keep the heap element type as `Int`: each heap
// item is a packed integer
//   `value * 3 + streamIndex`  (3 = stream count, hardcoded)
// where `value` is the actual element value. Decoding the value
// back out is just division/modulo. This avoids needing a
// `Pair<Int, Int>` or a `MutableList<IntArray>` (both of which trip
// generic-arg propagation issues that aren't critical to demonstrating
// the MinHeap itself).
//
// Values must be non-negative for the encoding to round-trip.

fun mergeThreeSortedStreams(a: IntArray, b: IntArray, c: IntArray): IntArray {
    val heap: MinHeap<Int> = MinHeap { x, y -> x - y }
    var ai = 0
    var bi = 0
    var ci = 0
    if (a.size > 0) heap.push(a[0] * 3 + 0)
    if (b.size > 0) heap.push(b[0] * 3 + 1)
    if (c.size > 0) heap.push(c[0] * 3 + 2)

    val total = a.size + b.size + c.size
    val out = IntArray(total)
    var k = 0
    while (!heap.isEmpty()) {
        val packed = heap.pop()
        val value = packed / 3
        val streamIdx = packed - value * 3
        out[k] = value
        k++
        if (streamIdx == 0) {
            ai++
            if (ai < a.size) heap.push(a[ai] * 3 + 0)
        } else if (streamIdx == 1) {
            bi++
            if (bi < b.size) heap.push(b[bi] * 3 + 1)
        } else {
            ci++
            if (ci < c.size) heap.push(c[ci] * 3 + 2)
        }
    }
    return out
}
