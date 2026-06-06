// Regression: a suspend function whose post-resume tail is a string
// template using a captured user parameter (e.g. `return "user#$id"`).
//
// Before the fix, the JVM backend's `emit_mir_segment` had no arm for
// `CallKind::MakeConcatWithConstants`, so the template Call was
// silently dropped. The function fell straight to its terminator,
// which loaded a slot that was never assigned and the JVM verifier
// rejected the method with "Bad local variable type — Type top
// (current frame, locals[N]) is not assignable to reference type".
import kotlinx.coroutines.*

suspend fun fetchUser(id: Int): String {
    delay(20)
    return "user#$id"
}

fun main() = runBlocking {
    println(fetchUser(7))
}
