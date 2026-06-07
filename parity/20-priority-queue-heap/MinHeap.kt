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
