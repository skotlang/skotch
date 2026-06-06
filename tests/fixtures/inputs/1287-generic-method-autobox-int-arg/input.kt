// Regression: calling a user-class method whose declared param
// types are type-parameters (`K`, `V`) erases the JVM descriptor to
// `(Object, Object)V`. The call site must autobox primitive args
// before invokevirtual — otherwise the JVM verifier rejects the
// bytecode with "Type integer is not assignable to 'java/lang/Object'".
//
// Pre-fix: `Box<String, Int>().put("a", 1)` lowered the `1` argument
// as an unboxed `iconst_1`, then invoked the put descriptor that
// expects two `Object`s. javap on the synthesized class showed the
// correct erased descriptor; the bug was at the call site only.
class Box<K, V> {
    private val store: MutableMap<K, V> = mutableMapOf()

    fun put(key: K, value: V) {
        store.put(key, value)
    }

    fun get(key: K): V? = store.get(key)
}

fun main() {
    val b = Box<String, Int>()
    b.put("x", 1)
    b.put("y", 2)
    println(b.get("x"))
    println(b.get("y"))
    println(b.get("z"))
}
