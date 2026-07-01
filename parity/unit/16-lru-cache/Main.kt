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
