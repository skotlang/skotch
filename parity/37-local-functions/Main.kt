// Drives the local-fn examples from Strings.kt and Trees.kt.
//
// reverse() exercises a 2-capture (builder + s) + recursion through
// a String.
//
// sumDigits() exercises a 1-capture (multiplier) + 2-explicit-param
// recursion (k, acc).
//
// heapSum() exercises a 2-capture (heap + size, both val) + a
// tree-recursive descent (visit left, then visit right). Threads
// the total through an explicit accumulator instead of a captured
// `var` because skotch doesn't yet Ref-box mutable captures (a
// v0.50 milestone gap).

fun main() {
    println(reverse("hello"))            // "olleh"
    println(reverse(""))                  // ""
    println(reverse("a"))                 // "a"
    println(reverse("kotlin"))            // "niltok"

    println(sumDigits(1234, 1))          // 10  (1+2+3+4)
    println(sumDigits(1234, 10))         // 100
    println(sumDigits(987654, 2))        // 78 (2*(9+8+7+6+5+4))
    println(sumDigits(0, 5))             // 0

    // heap: [10, 7, 8, 3, 4, 1, 2] — 7-element binary heap.
    // visit walks: root=10, left=7 → [3,4], right=8 → [1,2].
    // sum = 10+7+8+3+4+1+2 = 35.
    val heap = intArrayOf(10, 7, 8, 3, 4, 1, 2)
    println(heapSum(heap, heap.size))    // 35
    println(heapSum(heap, 1))             // 10 (just root)
    println(heapSum(heap, 3))             // 25 (root + 2 children: 10+7+8)
}
