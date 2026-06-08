// Arithmetic helper that throws on out-of-range input. Exercises:
//   - `throw IllegalArgumentException("msg")` with String-template
//     message construction (string + concat)
//   - `when` with multi-value branches (`1, 2, 3 -> "small"`) and
//     `in` range conditions (`in 2..10 -> "small"`)
//
// The when as a function body (`fun X(): T = when {...}`) lowers
// the entire body to the when's value — common Kotlin idiom.
//
// NOTE: skotch's parser doesn't accept NEGATIVE literals as
// when-branch values (`-1, 1` fails because `-1` parses as unary
// minus + literal, not a single value). Positive multi-value
// branches work. The `in -99..-2` range works because the range
// constructor handles unary minus at its arg positions.

fun classify(n: Int): String = when (n) {
    0 -> "zero"
    1 -> "edge"
    in 2..10 -> "small"
    in 11..99 -> "medium"
    in -99..-2 -> "negative"
    else -> "huge"
}

fun divide(a: Int, b: Int): Int {
    if (b == 0) {
        throw IllegalArgumentException("divide by zero: " + a)
    }
    return a / b
}

fun checkRange(n: Int, lo: Int, hi: Int): Int {
    if (n < lo || n > hi) {
        throw IllegalArgumentException("out of range: " + n)
    }
    return n
}
