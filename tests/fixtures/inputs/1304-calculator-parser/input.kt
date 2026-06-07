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
// Recursive-descent parser implementing the standard arithmetic
// precedence grammar:
//
//   expr   ::= term  (('+' | '-') term)*
//   term   ::= unary (('*' | '/') unary)*
//   unary  ::= '-' unary | atom
//   atom   ::= NUM | '(' expr ')'
//
// Implemented as top-level functions taking and returning a (Expr,
// nextCursor) tuple — encoded as `ParseResult` to avoid `Pair<Expr,
// Int>` (whose generic erasure trips skotch's checkcast/unbox path).
//
// AST is a sealed class so eval() can dispatch via `when (e) is X`.
sealed class Expr {
    abstract fun eval(): Int
    abstract fun show(): String
}

class Num(val n: Int) : Expr() {
    override fun eval(): Int = n
    override fun show(): String = "$n"
}

class BinOp(val op: String, val l: Expr, val r: Expr) : Expr() {
    override fun eval(): Int {
        val lv = l.eval()
        val rv = r.eval()
        if (op == "+") return lv + rv
        if (op == "-") return lv - rv
        if (op == "*") return lv * rv
        return lv / rv
    }

    override fun show(): String {
        return "(" + l.show() + " " + op + " " + r.show() + ")"
    }
}

class UnaryNeg(val x: Expr) : Expr() {
    override fun eval(): Int = -x.eval()
    override fun show(): String = "(-" + x.show() + ")"
}

// Carries a parsed sub-expression plus the cursor position immediately
// after it. Two concrete-typed fields — no generic erasure issues.
class ParseResult(val expr: Expr, val next: Int)

fun parse(tokens: MutableList<Token>): Expr {
    val r = parseExpr(tokens, 0)
    return r.expr
}

fun parseExpr(tokens: MutableList<Token>, start: Int): ParseResult {
    var lr = parseTerm(tokens, start)
    while (isAddOp(tokens[lr.next])) {
        val op = if (tokens[lr.next] is PlusTok) "+" else "-"
        val rr = parseTerm(tokens, lr.next + 1)
        lr = ParseResult(BinOp(op, lr.expr, rr.expr), rr.next)
    }
    return lr
}

fun parseTerm(tokens: MutableList<Token>, start: Int): ParseResult {
    var lr = parseUnary(tokens, start)
    while (isMulOp(tokens[lr.next])) {
        val op = if (tokens[lr.next] is StarTok) "*" else "/"
        val rr = parseUnary(tokens, lr.next + 1)
        lr = ParseResult(BinOp(op, lr.expr, rr.expr), rr.next)
    }
    return lr
}

fun parseUnary(tokens: MutableList<Token>, start: Int): ParseResult {
    val t = tokens[start]
    if (t is MinusTok) {
        val inner = parseUnary(tokens, start + 1)
        return ParseResult(UnaryNeg(inner.expr), inner.next)
    }
    return parseAtom(tokens, start)
}

fun parseAtom(tokens: MutableList<Token>, start: Int): ParseResult {
    val t = tokens[start]
    if (t is NumTok) {
        return ParseResult(Num(t.value), start + 1)
    }
    if (t is LParenTok) {
        val inner = parseExpr(tokens, start + 1)
        return ParseResult(inner.expr, inner.next + 1) // skip ')'
    }
    return ParseResult(Num(0), start)
}

fun isAddOp(t: Token): Boolean {
    return t is PlusTok || t is MinusTok
}

fun isMulOp(t: Token): Boolean {
    return t is StarTok || t is SlashTok
}
// Run the lexer + parser + evaluator on a small set of expressions.
// Output `<source> => <ast> = <value>` for each.

fun evaluate(src: String): String {
    val tokens = tokenize(src)
    val ast = parse(tokens)
    return src + " => " + ast.show() + " = " + ast.eval()
}

fun main() {
    println(evaluate("1 + 2"))
    println(evaluate("3 * 4"))
    println(evaluate("1 + 2 * 3"))
    println(evaluate("(1 + 2) * 3"))
    println(evaluate("10 - 4 - 3"))
    println(evaluate("8 / 2 / 2"))
    println(evaluate("-5 + 7"))
    println(evaluate("2 * -3"))
    println(evaluate("(1 + 2) * (3 + 4)"))
    println(evaluate("100 / (2 + 3) - 5"))
    println(evaluate("-(2 + 3)"))
}
