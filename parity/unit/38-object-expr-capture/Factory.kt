// Returns object-expression instances. Both helpers exercise
// captured local vals: `prefix` and `suffix` (Strings, ref capture),
// `factor` (Int, primitive capture). The synthesized anonymous class
// must store each capture as a field, accept them in the constructor,
// and load them via `getfield` inside the override method body.

fun makeProducer(prefix: String, suffix: String): Producer {
    return object : Producer {
        override fun produce(): String = prefix + suffix
    }
}

fun makeScaler(factor: Int): BinaryOp {
    return object : BinaryOp {
        override fun apply(a: Int, b: Int): Int = (a + b) * factor
    }
}
