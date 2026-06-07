// Regression: `sb.append(c)` where `c: Char` resolved to
// `StringBuilder.append(Ljava/lang/Object;)` instead of
// `StringBuilder.append(C)`. The JVM verifier rejected because the
// caller pushed an unboxed int where the Object overload expected a
// reference.
//
// Fix: extend `overload_score` so primitive arg types score 3 on the
// matching JVM character — `(Some(Ty::Char), "C") => 3`,
// `(Some(Ty::Byte), "B") => 3`, `(Some(Ty::Short), "S") => 3`. Each
// also widens to the int-shaped slot for `(_, "I")` matches.
fun main() {
    val sb = StringBuilder()
    val c: Char = 'x'
    sb.append(c)
    sb.append('Y')
    println(sb.toString())
}
