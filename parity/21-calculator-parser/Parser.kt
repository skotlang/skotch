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
