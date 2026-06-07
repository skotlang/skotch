// Mini regex matcher: sealed `Re` AST + recursive backtracking
// matcher. Supports a tiny subset of regex syntax:
//   single char, `.` (any char), concatenation, `|` alternation,
//   `*` Kleene star, and `()` grouping.
//
// Sophistication step over example 24:
//   - sealed-class AST with five subclasses (CharRe, DotRe, Concat,
//     Alt, Star), dispatched via `if (re is X)` chains in the matcher
//   - hand-rolled recursive-descent regex parser (separate from
//     example 21's calculator parser — different grammar, no precedence
//     climbing, but more sealed-class allocation)
//   - greedy + backtracking match semantics: `Star` tries the longest
//     repeat first, then peels back one match at a time if the rest
//     of the pattern doesn't match

sealed class Re
class CharRe(val c: Char) : Re()
object DotRe : Re()
class Concat(val a: Re, val b: Re) : Re()
class Alt(val a: Re, val b: Re) : Re()
class Star(val inner: Re) : Re()

// Try to match `re` starting at `pos`; return the new position on
// success (>= pos) or -1 on failure. For `Star`, returns the longest
// match — callers using Star inside Concat backtrack via
// `matchStarThen`.
//
// Single-return-at-end shape to avoid skotch's use-before-def
// analysis stubbing the whole method when multiple early returns
// fan out of `if (re is X)` branches.
fun matchAt(re: Re, input: String, pos: Int): Int {
    if (re is CharRe) return matchChar(re, input, pos)
    if (re is DotRe) return matchDot(input, pos)
    if (re is Concat) return matchConcat(re.a, re.b, input, pos)
    if (re is Alt) return matchAlt(re.a, re.b, input, pos)
    if (re is Star) return matchStarGreedy(re.inner, input, pos)
    return -1
}

fun matchChar(re: CharRe, input: String, pos: Int): Int {
    if (pos < input.length && input[pos] == re.c) return pos + 1
    return -1
}

fun matchDot(input: String, pos: Int): Int {
    if (pos < input.length) return pos + 1
    return -1
}

fun matchAlt(a: Re, b: Re, input: String, pos: Int): Int {
    val tryA = matchAt(a, input, pos)
    if (tryA >= 0) return tryA
    return matchAt(b, input, pos)
}

// Greedy longest match — used when Star is the OUTER re. Callers
// chaining Star with a successor use `matchStarThen` for backtracking.
fun matchStarGreedy(inner: Re, input: String, pos: Int): Int {
    var cur = pos
    var done = false
    while (!done) {
        val next = matchAt(inner, input, cur)
        if (next < 0 || next == cur) {
            done = true
        } else {
            cur = next
        }
    }
    return cur
}

// Try matching `a` followed by `b`. If `a` is a Star, we backtrack
// across the Star's repetition count: try the longest match for the
// Star first, then shorter and shorter if `b` doesn't match what's
// left. This is the standard greedy-with-backtracking approach.
fun matchConcat(a: Re, b: Re, input: String, pos: Int): Int {
    if (a is Star) {
        return matchStarThen(a.inner, b, input, pos)
    }
    val mid = matchAt(a, input, pos)
    if (mid < 0) return -1
    return matchAt(b, input, mid)
}

// `inner*` followed by `rest`. Greedy: count the max k such that
// `inner` matches k times at `pos`, then try `rest` after k matches,
// then k-1, then k-2, ... down to 0.
fun matchStarThen(inner: Re, rest: Re, input: String, pos: Int): Int {
    val ends = collectStarEnds(inner, input, pos)
    var i = ends.size - 1
    while (i >= 0) {
        val tryEnd = matchAt(rest, input, ends[i])
        if (tryEnd >= 0) return tryEnd
        i = i - 1
    }
    return -1
}

// Collect every position reachable by matching `inner` zero or more
// times starting at `pos`. The first entry is always `pos` (zero
// matches); each subsequent entry is one more iteration of `inner`.
fun collectStarEnds(inner: Re, input: String, pos: Int): IntArray {
    val capacity = input.length - pos + 1
    val buf = IntArray(capacity + 1)
    buf[0] = pos
    var count = 1
    var cur = pos
    while (true) {
        val next = matchAt(inner, input, cur)
        if (next < 0 || next == cur) {
            val out = IntArray(count)
            var k = 0
            while (k < count) {
                out[k] = buf[k]
                k = k + 1
            }
            return out
        }
        buf[count] = next
        count = count + 1
        cur = next
    }
}

// Top-level: does `re` match the ENTIRE input?
fun matches(re: Re, input: String): Boolean {
    return matchAt(re, input, 0) == input.length
}
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
// Run several pattern + input pairs through the parser+matcher and
// print whether each matched.

fun describe(pattern: String, input: String): String {
    val re = parseRegex(pattern)
    val ok = matches(re, input)
    val result = if (ok) "match" else "no match"
    return "/$pattern/ vs \"$input\" => $result"
}

fun main() {
    println(describe("a", "a"))
    println(describe("a", "b"))
    println(describe("ab", "ab"))
    println(describe("ab", "a"))
    println(describe("a|b", "a"))
    println(describe("a|b", "b"))
    println(describe("a|b", "c"))
    println(describe("a*", ""))
    println(describe("a*", "aaa"))
    println(describe("a*", "aab"))
    println(describe("a*b", "b"))
    println(describe("a*b", "aaab"))
    println(describe("a*b", "aaa"))
    println(describe(".", "x"))
    println(describe(".*", "anything goes"))
    println(describe("a.*b", "axxxb"))
    println(describe("(ab)*", "ababab"))
    println(describe("(ab)*c", "ababc"))
    println(describe("(a|b)*c", "aabbabc"))
    println(describe("(a|b)*c", "abxc"))
}
