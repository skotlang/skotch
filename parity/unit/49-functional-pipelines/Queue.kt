// FIFO queue probing the `val r = field; field = ...; return r`
// val-aliasing gap (known v0.50). Pop() reads `head`'s value into
// `r`, mutates `head` to head.next, then returns `r`. Without the
// fix, skotch reuses r's slot for the post-mutation head value, so
// the returned value is wrong.

class Node<T>(val value: T, var next: Node<T>?)

class Queue<T> {
    var head: Node<T>? = null
    var tail: Node<T>? = null
    var size: Int = 0

    fun enqueue(value: T) {
        val n = Node(value, null)
        val t = tail
        if (t == null) {
            head = n
        } else {
            t.next = n
        }
        tail = n
        size = size + 1
    }

    // Probes val-aliasing: `r` should be the SNAPSHOT of head.value
    // taken BEFORE head is mutated.
    fun dequeue(): T? {
        val h = head ?: return null  // also probes Elvis-then-return
        val r = h.value              // VAL-ALIASING TARGET
        head = h.next
        if (head == null) tail = null
        size = size - 1
        return r
    }
}
