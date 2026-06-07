// Indexed-assignment dispatch for stdlib collections.
//
// Pre-fix: `map[k] = v` and `list[i] = v` both emitted `aastore`
// (array store), which the JVM verifier rejected with "Bad type on
// operand stack" because `Map`/`List` are reference types, not
// arrays. The MIR-lower `Stmt::IndexAssign` only knew about
// user-class operator `set` and arrays; collection dispatch was
// missing.
//
// Fix: extend `Stmt::IndexAssign` to detect `Ty::Class(cn)` where
// the class name contains "Map" → `invokeinterface Map.put(K, V)`
// (autoboxing the key and value), and "List" → `invokeinterface
// List.set(int, V)` (autoboxing the value). Discard the put/set
// return value (the old V) via the dest local.

fun main() {
    val map: MutableMap<Int, Int> = mutableMapOf()
    map[1] = 10
    map[2] = 20
    map[3] = 30
    println(map[1])
    println(map[2])
    println(map[3])

    val list: MutableList<Int> = mutableListOf(0, 0, 0)
    list[0] = 100
    list[1] = 200
    list[2] = 300
    println(list[0])
    println(list[1])
    println(list[2])
}
