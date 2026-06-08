// `crossinline` modifier on inline-function lambda params. Required
// when the lambda must escape the inline function's local scope —
// e.g. captured by a Runnable or stored in a field. The modifier
// disallows non-local returns from the lambda body.
//
// Skotch's status: `crossinline` keyword IS recognized by the lexer
// (per skotch-repl/src/completion.rs) but not actually parsed as a
// modifier on lambda params. Probable failure: parser treats
// `crossinline` as an identifier in param position.

inline fun later(crossinline body: () -> Unit) {
    val r = Runnable {
        body()
    }
    r.run()
}

// `noinline` is the dual — the lambda is NOT inlined, kept as a
// Function object that can be passed around like any other.
inline fun applyOnce(noinline f: (Int) -> Int, x: Int): Int {
    return f(x)
}
