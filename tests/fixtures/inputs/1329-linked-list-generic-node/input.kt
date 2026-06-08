// Generic `Node<T>` with self-referential nullable `next: Node<T>?`
// + a LinkedList<T> wrapper. Probes:
//   - generic class with self-referential nullable property type
//   - var head: Node<T>? (mutable nullable on a generic class)
//   - smart cast in `if (h == null) return null`
//   - while-loop walking nullable chain
//
// Several known-deferred gaps surfaced during the parity probe
// (parity/43-linked-list):
//   - `head ?: return null` — Elvis-then-return not parsed
//   - `val h = head; head = h.next; return h.value` — val-aliasing
//     bug aliases h to the new head's slot
//   - `sum + nullable_int` — smart-cast to non-null Int doesn't
//     auto-unbox
// Workarounds in this fixture: split assignments to snapshot reads
// before mutation, explicit null check, bounded loop instead of
// `while (size > 0)`.

class Node<T>(val value: T, var next: Node<T>?)

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

fun main() {
    val list = LinkedList<String>()
    list.push("a")
    list.push("b")
    list.push("c")
    println(list.size())
    println(list.pop())
    println(list.pop())
    println(list.size())
    println(list.pop())
    println(list.pop())
    println(list.size())
}
