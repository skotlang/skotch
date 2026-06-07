// Regression: `try_java_static_call` lowered call args eagerly while
// probing whether the receiver+method matched a Java static call. On
// miss, it returned None, but the args had already been emitted as
// MIR Call statements (with side effects on the FnBuilder). The
// caller's fall-through to regular instance-method dispatch then
// lowered the SAME args AGAIN, producing two `invokevirtual` calls
// where there should be one. Symptom: `list.add(makeValue())` where
// `makeValue()` advances an internal cursor invoked `makeValue` twice
// per iteration, doubling the cursor movement and corrupting parser
// state.
//
// Fix: short-circuit `try_java_static_call` with a side-effect-free
// `lookup_java_static_typed(class, method, args.len(), &[])` probe;
// only lower args after the probe confirms a candidate exists.
// Restrict the probe to dotted-name receivers (`java.lang.System`)
// to leave bare PascalCase identifiers (user classes / instance
// receivers) on the regular dispatch path.

class P(val src: String) {
    var pos: Int = 0

    fun makeValue(): String {
        val c = src[pos]
        pos++
        return "${c}"
    }

    fun parseAll(): MutableList<String> {
        val items = mutableListOf<String>()
        while (pos < src.length) {
            items.add(makeValue())
        }
        return items
    }
}

fun main() {
    val p = P("ab")
    val items = p.parseAll()
    println("size=${items.size}")
}
