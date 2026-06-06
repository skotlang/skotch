// Regression for audit finding #7: enums can implement interfaces,
// per-entry overrides land as virtual methods on the enum class,
// AND chained `EnumName.ENTRY.method(args)` resolves to the
// per-entry virtual dispatcher (was returning null because the
// dispatch_name lookup only handled abstract-on-enum-body methods).
interface Calculable {
    fun apply(a: Int, b: Int): Int
}

enum class Op : Calculable {
    PLUS {
        override fun apply(a: Int, b: Int): Int = a + b
    },
    TIMES {
        override fun apply(a: Int, b: Int): Int = a * b
    };
}

fun main() {
    // Through an interface-typed reference.
    val plus: Calculable = Op.PLUS
    val times: Calculable = Op.TIMES
    println(plus.apply(2, 3))
    println(times.apply(5, 6))
    // Chained direct-on-entry call — used to drop to a null
    // placeholder before the virtual-dispatcher fallback landed.
    println(Op.PLUS.apply(10, 20))
    println(Op.TIMES.apply(4, 8))
}
