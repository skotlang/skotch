// `"""..."""` raw strings preserve newlines and don't interpret
// escapes (`\n` is literally backslash+n). Pre-fix skotch's
// raw-string lexer treated `$` as a literal character, so
// `$name` and `${expr}` came out verbatim.
//
// Fix (lexer/lib.rs:~728): the raw-string scanner now mirrors the
// regular-string $-handling — `$ident` and `${expr}` both produce
// interpolated values. `\n`, `\t`, etc. STILL pass through
// verbatim (raw-string semantics preserved); only `$` gets
// re-routed through the interpolation machinery.

fun greet(name: String, age: Int): String = """
        Hello, $name!
        You are $age years old.
        ${if (age >= 18) "Status: adult." else "Status: minor."}
    """.trimIndent()

fun renderRow(label: String, value: Int, total: Int): String {
    val pct = (value * 100) / total
    return """
        | $label: $value/$total ($pct%)
    """.trimIndent()
}
