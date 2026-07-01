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
