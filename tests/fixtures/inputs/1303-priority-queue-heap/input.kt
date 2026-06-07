// Generic min-heap (binary heap) parameterized by an item type `T`
// and a comparator function `(T, T) -> Int`. Returns < 0 if a < b.
//
// Sophistication step over example 19:
//   - generic class with a function-typed property (comparator HOF)
//   - recursive heap structure implemented iteratively via index math
//   - mutates a MutableList<T> in place via index assignment + removeAt
//   - swap with a temporary `tmp` (no destructuring assignment)
//   - exercises a method whose return type IS the class's type parameter
//     `T` (`pop(): T`) — call sites need the concrete element type to
//     dispatch downstream field/index access correctly
class MinHeap<T>(private val compare: (T, T) -> Int) {
    private val items: MutableList<T> = mutableListOf()

    fun size(): Int = items.size

    fun isEmpty(): Boolean = items.size == 0

    fun push(item: T) {
        items.add(item)
        siftUp(items.size - 1)
    }

    fun pop(): T {
        val top = items[0]
        val last = items.size - 1
        if (last == 0) {
            items.removeAt(0)
            return top
        }
        items[0] = items[last]
        items.removeAt(last)
        siftDown(0)
        return top
    }

    fun peek(): T {
        return items[0]
    }

    private fun siftUp(idx: Int) {
        var i = idx
        while (i > 0) {
            val parent = (i - 1) / 2
            if (compare(items[i], items[parent]) < 0) {
                val tmp = items[i]
                items[i] = items[parent]
                items[parent] = tmp
                i = parent
            } else {
                return
            }
        }
    }

    private fun siftDown(idx: Int) {
        var i = idx
        val n = items.size
        while (true) {
            val left = 2 * i + 1
            val right = 2 * i + 2
            var smallest = i
            if (left < n && compare(items[left], items[smallest]) < 0) {
                smallest = left
            }
            if (right < n && compare(items[right], items[smallest]) < 0) {
                smallest = right
            }
            if (smallest == i) {
                return
            }
            val tmp = items[i]
            items[i] = items[smallest]
            items[smallest] = tmp
            i = smallest
        }
    }
}
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
// Three demos of the MinHeap from MinHeap.kt:
//   1. Heap-sort N ints
//   2. Find the K largest elements from a stream of ints
//   3. K-way merge of sorted IntArrays

fun heapSort(input: IntArray): IntArray {
    val heap: MinHeap<Int> = MinHeap { a, b -> a - b }
    var i = 0
    while (i < input.size) {
        heap.push(input[i])
        i++
    }
    val out = IntArray(input.size)
    var j = 0
    while (!heap.isEmpty()) {
        out[j] = heap.pop()
        j++
    }
    return out
}

// Keep the K LARGEST elements seen so far in a min-heap of size K.
// The root is the smallest of the kept set — a new candidate replaces
// it only when it's larger.
fun topKLargest(input: IntArray, k: Int): IntArray {
    val heap: MinHeap<Int> = MinHeap { a, b -> a - b }
    var i = 0
    while (i < input.size) {
        val v = input[i]
        if (heap.size() < k) {
            heap.push(v)
        } else if (v > heap.peek()) {
            heap.pop()
            heap.push(v)
        }
        i++
    }
    // Drain (ascending) into a buffer, then reverse so output is
    // descending — the largest first.
    val asc = IntArray(heap.size())
    var j = 0
    while (!heap.isEmpty()) {
        asc[j] = heap.pop()
        j++
    }
    val out = IntArray(asc.size)
    var k1 = 0
    while (k1 < asc.size) {
        out[k1] = asc[asc.size - 1 - k1]
        k1++
    }
    return out
}

fun main() {
    val unsorted = intArrayOf(7, 2, 9, 1, 5, 3, 8, 4, 6)
    val sorted = heapSort(unsorted)
    var i = 0
    while (i < sorted.size) {
        println(sorted[i])
        i++
    }

    println("---")

    val top3 = topKLargest(unsorted, 3)
    var j = 0
    while (j < top3.size) {
        println(top3[j])
        j++
    }

    println("---")

    printMerged()
}

fun printMerged() {
    val a = intArrayOf(1, 4, 7, 10)
    val b = intArrayOf(2, 5, 8, 11)
    val c = intArrayOf(3, 6, 9, 12)
    val merged = mergeThreeSortedStreams(a, b, c)
    var k = 0
    while (k < merged.size) {
        println(merged[k])
        k++
    }
}
