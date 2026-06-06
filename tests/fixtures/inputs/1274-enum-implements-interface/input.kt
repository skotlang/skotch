// Regression for audit finding #7: enums can implement interfaces.
// Previously EnumDecl had no `interfaces` field — the parser silently
// dropped `enum class Op : Calculable`. Now the AST carries the
// interface list and the JVM class file lists the interface in its
// `interfaces` table.
//
// The interface's method has a default body so per-entry overrides
// (which skotch doesn't lower as anonymous-class subclasses today)
// aren't needed — the test isolates the audit fix from that
// separate gap.
interface Calculable {
    fun apply(a: Int, b: Int): Int = a + b
}

enum class Op : Calculable {
    PLUS, MINUS;
}

fun main() {
    val plus: Calculable = Op.PLUS
    val minus: Calculable = Op.MINUS
    println(plus.apply(2, 3))
    println(minus.apply(7, 4))
}
