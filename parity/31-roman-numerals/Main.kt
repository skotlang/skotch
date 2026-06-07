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
