// Generic LRU (least-recently-used) cache with explicit eviction.
//
// Sophistication over example 15:
//   - two independent type parameters (`K`, `V`)
//   - MutableMap<K, V> backing store + parallel MutableList<K> ordering
//   - both are touched on every read AND every write — exercises the
//     compiler's handling of generic-stdlib method dispatch on multiple
//     stdlib collection types in the same body
//   - the `put` path branches on `data.containsKey(key)` AND on
//     capacity, then evicts via `order.removeAt(0)` followed by
//     `data.remove(evictedKey)`
//   - real-world non-toy semantics: get-bumps-to-most-recent, put-of-
//     existing-key replaces in place, put-when-full evicts the oldest.
class LruCache<K, V>(val capacity: Int) {
    private val data: MutableMap<K, V> = mutableMapOf()
    private val order: MutableList<K> = mutableListOf()

    fun get(key: K): V? {
        val value = data.get(key)
        if (value != null) {
            order.remove(key)
            order.add(key)
        }
        return value
    }

    fun put(key: K, value: V) {
        if (data.containsKey(key)) {
            order.remove(key)
        } else if (data.size >= capacity) {
            val evicted = order.removeAt(0)
            data.remove(evicted)
        }
        data.put(key, value)
        order.add(key)
    }

    fun size(): Int = data.size

    // Returns the keys in least-recently-used → most-recently-used
    // order. The caller gets a snapshot — internal mutation after this
    // call doesn't affect the returned list.
    fun keys(): List<K> {
        val out = mutableListOf<K>()
        var i = 0
        while (i < order.size) {
            out.add(order[i])
            i++
        }
        return out
    }
}
