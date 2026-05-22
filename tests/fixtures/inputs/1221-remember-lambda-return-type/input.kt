// Locks in: a generic-T-from-lambda stdlib call (modeled after
// Compose's `remember { ... }`) infers T from the lambda body's
// return type. Without inference, the result is `Any` and downstream
// member access fails.
//
// We can't use the real `androidx.compose.runtime.remember` because
// the e2e classpath doesn't load Compose runtime — but we model the
// shape with a local generic function. The MIR-lower path for
// `remember`/`lazy`/`derivedStateOf` then exercises the same
// trailing-lambda-return-type inference, with the result type
// flowing into the caller's typed slot.

data class Box(val value: Int)

fun <T> rememberLocal(calc: () -> T): T = calc()

fun main() {
    val b = rememberLocal { Box(42) }
    println(b.value)
}
