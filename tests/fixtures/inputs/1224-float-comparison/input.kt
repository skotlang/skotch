// Float relational comparisons must lower to `fcmpl`/`fcmpg` + `if<cond>`,
// not the integer `if_icmp` path — the latter is rejected by the JVM verifier
// and d8 ("Expected primitive int on stack, but was primitive float").
// Surfaced by JetChat's `Dp`/Float comparisons (e.g. `bottomOffset > 0.dp`).
fun gt(a: Float, b: Float): Boolean = a > b

fun le(a: Float, b: Float): Boolean = a <= b

fun main() {
    println(gt(2.0f, 1.0f))
    println(le(2.0f, 1.0f))
    println(gt(1.0f, 1.0f))
}
