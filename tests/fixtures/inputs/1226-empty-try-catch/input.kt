// An empty try body cannot throw, so catch/finally handlers around it
// must not produce a zero-length protected region in the exception table.
// A zero-length entry (start_pc == end_pc) is malformed: d8 crashes with
// an ArrayIndexOutOfBoundsException and the JVM verifier rejects it.
// kotlinc emits no handler for an empty try; skotch must do the same.

fun emptyCatch() {
    try {
    } catch (e: Exception) {
        throw e
    }
}

fun emptyFinally() {
    try {
    } finally {
        println("finally ran")
    }
}

fun emptyCatchFinally() {
    try {
    } catch (e: Exception) {
        throw e
    } finally {
        println("cf finally ran")
    }
}

fun main() {
    emptyCatch()
    emptyFinally()
    emptyCatchFinally()
    println("ok")
}
