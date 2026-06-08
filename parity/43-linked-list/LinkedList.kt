// LIFO linked list (stack) built on Node<T>. Probes:
//   - var head: Node<T>? — mutable nullable property on a generic class
//   - head = Node(value, head) — self-referencing constructor call
//   - nullable smart-cast inside `if (h == null) return null`
//   - while-loop over nullable next pointer
//   - cross-file generic class composition (Node lives in Node.kt)
//
// WORKAROUNDS (documented v0.50 gaps):
//   - `head ?: return null` — Elvis-then-return not parsed (return
//     isn't an expression). Rewrite as `if (h == null) return null`.
//   - `val h = head; head = h.next; return h.value` — val-aliasing
//     bug: skotch reuses h's slot for the new `head` assignment, so
//     `h.value` at the end reads from the NEW head, not the snapshot.
//     Workaround: read `h.value` into a `val result` BEFORE mutating
//     head, then return `result`.

class LinkedList<T> {
    var head: Node<T>? = null

    fun push(value: T) {
        head = Node(value, head)
    }

    fun pop(): T? {
        val h = head
        if (h == null) return null
        val result = h.value
        head = h.next
        return result
    }

    fun size(): Int {
        var count = 0
        var cur = head
        while (cur != null) {
            count = count + 1
            cur = cur.next
        }
        return count
    }
}
