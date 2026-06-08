// `crossinline` and `noinline` modifiers on inline-function lambda
// params. Pre-fix the parser's param reader (parser.rs:~1976) only
// looked for the optional `vararg` modifier before the param name —
// `crossinline` and `noinline` (Ident tokens, not lexer keywords)
// trickled into the param parser as bogus name candidates and
// broke the rest of the file's parse.
//
// Fix at parser.rs:~1989 accepts `crossinline`/`noinline` Ident
// payloads as modifiers before the param name. skotch doesn't yet
// enforce the JVM-level semantics; the parser just consumes the
// keywords so the rest of the file parses.
//
// This fixture exercises the PARSER fix only — body returns a
// pure value, no var-capture (which is a separate gap).

inline fun later(crossinline body: () -> String): String {
    return body()
}

inline fun applyOnce(noinline f: (Int) -> Int, x: Int): Int {
    return f(x)
}

fun main() {
    val result = later { "ran" }
    println(result)
    val doubled = applyOnce({ x -> x * 2 }, 21)
    println(doubled)
}
