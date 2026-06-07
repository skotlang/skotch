// Lexer: turn a source string like "1 + 2 * (3 - 4)" into a list of
// Tokens. Tokens are encoded as a sealed class so the parser can do
// `when (t) is NumTok -> ...` dispatch.
//
// The lexer is implemented as TOP-LEVEL functions (no class with
// mutable `pos`) — skotch currently has var-field staleness issues
// that show up when a class method updates `var pos` from inside a
// while loop and a helper reads it. A pure-function version threads
// the position through return values instead.

sealed class Token
class NumTok(val value: Int) : Token()
object PlusTok : Token()
object MinusTok : Token()
object StarTok : Token()
object SlashTok : Token()
object LParenTok : Token()
object RParenTok : Token()
object EofTok : Token()

fun tokenize(src: String): MutableList<Token> {
    val out: MutableList<Token> = mutableListOf()
    var i = 0
    while (i < src.length) {
        i = stepOnce(src, i, out)
    }
    out.add(EofTok)
    return out
}

// Reads one token (or skips whitespace) starting at `i`, appends to
// `out` if a token is found, and returns the new cursor position.
fun stepOnce(src: String, i: Int, out: MutableList<Token>): Int {
    val c = src[i]
    val single = singleCharToken(c)
    if (single != null) {
        out.add(single)
        return i + 1
    }
    if (c >= '0' && c <= '9') {
        return readNumber(src, i, out)
    }
    return i + 1
}

fun singleCharToken(c: Char): Token? {
    if (c == '+') return PlusTok
    if (c == '-') return MinusTok
    if (c == '*') return StarTok
    if (c == '/') return SlashTok
    if (c == '(') return LParenTok
    if (c == ')') return RParenTok
    return null
}

fun readNumber(src: String, start: Int, out: MutableList<Token>): Int {
    val sb = StringBuilder()
    var i = start
    while (i < src.length) {
        val c = src[i]
        if (c >= '0' && c <= '9') {
            sb.append(c)
            i++
        } else {
            break
        }
    }
    out.add(NumTok(sb.toString().toInt()))
    return i
}
