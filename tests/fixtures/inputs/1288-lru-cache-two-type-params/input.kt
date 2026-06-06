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
fun describe(cache: LruCache<String, Int>, label: String) {
    println("${label}: keys=${cache.keys()} size=${cache.size()}")
}

fun main() {
    val cache = LruCache<String, Int>(3)
    cache.put("a", 1)
    cache.put("b", 2)
    cache.put("c", 3)
    describe(cache, "initial fill")

    // get(a) bumps `a` to the most-recently-used end.
    val a = cache.get("a")
    println("get(a) = ${a}")
    describe(cache, "after get(a)")

    // put(d) evicts the LRU entry (which is now `b` after the bump).
    cache.put("d", 4)
    describe(cache, "after put(d)")
    println("get(b) (evicted) = ${cache.get("b")}")
    println("get(d) = ${cache.get("d")}")

    // Re-put an existing key — should replace value in place, not evict.
    cache.put("a", 99)
    describe(cache, "after put(a=99)")
    println("get(a) = ${cache.get("a")}")
}
