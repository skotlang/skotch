// Regression: per-entry overrides on an enum class now land as real
// virtual methods on the enum class itself (not just as a top-level
// `EnumName$methodName` dispatch wrapper). Without this, interface-
// or supertype-typed call sites that route through invokevirtual /
// invokeinterface hit AbstractMethodError because the JVM can't find
// the method on the enum class via the class-hierarchy walk.
//
// The dispatcher body is the same if-chain shape as the top-level
// wrapper — what changed is that the method now lives on the class.
interface Calc {
    fun apply(a: Int, b: Int): Int
    fun label(): String
}

enum class Op : Calc {
    ADD {
        override fun apply(a: Int, b: Int): Int = a + b
        override fun label(): String = "add"
    },
    MUL {
        override fun apply(a: Int, b: Int): Int = a * b
        override fun label(): String = "mul"
    };
}

fun runIt(c: Calc, a: Int, b: Int): String =
    "${c.label()}($a,$b) = ${c.apply(a, b)}"

fun main() {
    val plus: Calc = Op.ADD
    val times: Calc = Op.MUL
    println(runIt(plus, 2, 3))
    println(runIt(times, 4, 5))
}
