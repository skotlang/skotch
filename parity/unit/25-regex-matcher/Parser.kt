// Recursive-descent regex parser. Grammar:
//
//   alt    ::= concat ('|' concat)*
//   concat ::= post post*           (juxtaposition)
//   post   ::= atom '*'?
//   atom   ::= CHAR | '.' | '(' alt ')'
//
// Parser threads a cursor through `ParseResult(re, next)` return
// values — same shape as example 21's calculator parser, avoiding
// var-field state on a Parser class.

class ParseResult(val re: Re, val next: Int)

fun parseRegex(src: String): Re {
    val r = parseAlt(src, 0)
    return r.re
}

fun parseAlt(src: String, start: Int): ParseResult {
    var lr = parseConcat(src, start)
    while (lr.next < src.length && src[lr.next] == '|') {
        val rr = parseConcat(src, lr.next + 1)
        lr = ParseResult(Alt(lr.re, rr.re), rr.next)
    }
    return lr
}

fun parseConcat(src: String, start: Int): ParseResult {
    var lr = parsePost(src, start)
    while (lr.next < src.length && !isAltOrCloseParen(src[lr.next])) {
        val rr = parsePost(src, lr.next)
        lr = ParseResult(Concat(lr.re, rr.re), rr.next)
    }
    return lr
}

fun parsePost(src: String, start: Int): ParseResult {
    val atom = parseAtom(src, start)
    if (atom.next < src.length && src[atom.next] == '*') {
        return ParseResult(Star(atom.re), atom.next + 1)
    }
    return atom
}

fun parseAtom(src: String, start: Int): ParseResult {
    val c = src[start]
    if (c == '(') {
        val inner = parseAlt(src, start + 1)
        return ParseResult(inner.re, inner.next + 1) // skip ')'
    }
    if (c == '.') {
        return ParseResult(DotRe, start + 1)
    }
    return ParseResult(CharRe(c), start + 1)
}

fun isAltOrCloseParen(c: Char): Boolean {
    return c == '|' || c == ')'
}
