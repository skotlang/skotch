// `"""..."""` raw-string interpolation: `$ident` and `${expr}`
// inside triple-quoted strings. Pre-fix the raw-string scanner at
// lexer/lib.rs:~728 treated `$` as a literal character (only single-
// quoted strings did interpolation). Result: `$name` appeared
// verbatim in the output instead of being replaced.
//
// Fix mirrors the single-quoted scan_string $-handling: when `$`
// is seen, flush the current chunk, then handle `${...}` (via
// scan_interpolated_expr) or `$ident` (StringIdentRef) or literal
// `$` (push to chunk). Newlines, backslashes, and other characters
// still pass through verbatim — raw-string semantics preserved.

fun greet(name: String, age: Int): String = """
    Hello, $name!
    You are $age years old.
    ${if (age >= 18) "adult" else "minor"}
""".trimIndent()

fun main() {
    println(greet("Alice", 30))
    println("---")
    println(greet("Bob", 16))
}
