// Regression: a function parameter typed `StringBuilder` (unqualified
// java.lang.StringBuilder) emitted descriptor `LStringBuilder;`
// instead of `Ljava/lang/StringBuilder;`. The JVM verifier hit
// `NoClassDefFoundError: StringBuilder` at the first call site
// because no class lives at the unqualified path.
//
// Fix: extend `well_known_class_name` with common java.lang.* /
// kotlin.* unqualified names, and consult it from the
// param-type fq-resolution pass in mir-lower's `lower_function`.

fun build(sb: StringBuilder, s: String) {
    sb.append(s)
}

fun main() {
    val sb = StringBuilder()
    build(sb, "hi")
    println(sb.toString())
}
