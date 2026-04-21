enum class Op {
    PLUS {
        override fun apply(a: Int, b: Int): Int = a + b
    },
    MINUS {
        override fun apply(a: Int, b: Int): Int = a - b
    };

    abstract fun apply(a: Int, b: Int): Int
}

fun main() {
    println(Op.PLUS.apply(3, 2))
    println(Op.MINUS.apply(3, 2))
}
