// Roman numeral conversion (Int ↔ String). Uses the standard
// greedy subtractive-notation table: at each step, append the
// largest symbol that fits and subtract its value.
//
// Sophistication step over example 30:
//   - parallel IntArray + per-index symbol dispatch (no
//     `Array<String>` which would erase + hit indexing issues)
//   - Char-keyed lookup via if-else chain for the inverse direction
//   - subtractive notation: the inverse walks the string two chars
//     at a time and subtracts when a smaller symbol precedes a
//     larger one (`IV → 4`, `IX → 9`, …)

fun symbolAt(i: Int): String {
    if (i == 0) return "M"
    if (i == 1) return "CM"
    if (i == 2) return "D"
    if (i == 3) return "CD"
    if (i == 4) return "C"
    if (i == 5) return "XC"
    if (i == 6) return "L"
    if (i == 7) return "XL"
    if (i == 8) return "X"
    if (i == 9) return "IX"
    if (i == 10) return "V"
    if (i == 11) return "IV"
    return "I"
}

fun toRoman(n: Int): String {
    val values = intArrayOf(1000, 900, 500, 400, 100, 90, 50, 40, 10, 9, 5, 4, 1)
    val sb = StringBuilder()
    var x = n
    var i = 0
    while (i < values.size) {
        while (x >= values[i]) {
            sb.append(symbolAt(i))
            x = x - values[i]
        }
        i = i + 1
    }
    return sb.toString()
}

fun romanValue(c: Char): Int {
    if (c == 'M') return 1000
    if (c == 'D') return 500
    if (c == 'C') return 100
    if (c == 'L') return 50
    if (c == 'X') return 10
    if (c == 'V') return 5
    return 1
}

fun fromRoman(s: String): Int {
    var total = 0
    var i = 0
    while (i < s.length) {
        val cur = romanValue(s[i])
        val next = nextRoman(s, i)
        if (cur < next) {
            total = total - cur
        } else {
            total = total + cur
        }
        i = i + 1
    }
    return total
}

fun nextRoman(s: String, i: Int): Int {
    if (i + 1 >= s.length) return 0
    return romanValue(s[i + 1])
}
// Round-trip test: convert Int → Roman → Int and print all three.

fun main() {
    val tests = intArrayOf(
        1, 2, 3, 4, 5, 9, 10, 14, 19,
        40, 50, 90, 99, 100, 400, 500, 900,
        1000, 1066, 1492, 1776, 1949, 2024, 3999
    )
    var i = 0
    while (i < tests.size) {
        val n = tests[i]
        val r = toRoman(n)
        val back = fromRoman(r)
        println("$n => $r => $back")
        i = i + 1
    }
}
