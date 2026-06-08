// Drives the raw-string templates from Template.kt. Each test
// exercises a different facet of the lexer fix:
//   - `$name` simple interpolation in `"""..."""`
//   - `$age` integer interpolation (auto-toString)
//   - `${if (...) "..." else "..."}` complex expression interpolation
//   - `.trimIndent()` chained call on the raw string
//   - Arithmetic inside `${}` (pct calculation)

fun main() {
    println(greet("Alice", 30))
    println("---")
    println(greet("Bob", 16))
    println("---")
    println(renderRow("apples", 5, 20))
    println(renderRow("oranges", 7, 10))
    println(renderRow("pears", 0, 8))
    println("---")

    // Multi-line literal with NO interpolation — confirms baseline
    // raw-string preservation still works (escapes don't fire).
    val literal = """
        backslash: \n stays as backslash-n
        tab: \t stays as backslash-t
        dollar in code: \$
    """.trimIndent()
    println(literal)
}
