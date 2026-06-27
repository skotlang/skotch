enum class Op(val sym: String) {
    ADD("+"), SUB("-"), MUL("*");

    fun apply(a: Int, b: Int): Int = when (this) {
        ADD -> a + b
        SUB -> a - b
        MUL -> a * b
    }
}

fun main() {
    for (op in Op.values()) {
        println("${op.sym}: ${op.apply(7, 3)}")
    }
}
