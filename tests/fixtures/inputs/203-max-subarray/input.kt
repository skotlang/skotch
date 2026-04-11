fun main() {
    // Kadane's algorithm on a hardcoded array
    val a1 = -2; val a2 = 1; val a3 = -3; val a4 = 4
    val a5 = -1; val a6 = 2; val a7 = 1; val a8 = -5; val a9 = 4

    var maxSoFar = a1
    var maxEndingHere = a1

    // Process remaining elements
    for (i in 2..9) {
        val ai = when (i) {
            2 -> a2; 3 -> a3; 4 -> a4; 5 -> a5
            6 -> a6; 7 -> a7; 8 -> a8; 9 -> a9
            else -> 0
        }
        if (maxEndingHere + ai > ai) {
            maxEndingHere = maxEndingHere + ai
        } else {
            maxEndingHere = ai
        }
        if (maxEndingHere > maxSoFar) {
            maxSoFar = maxEndingHere
        }
    }
    println(maxSoFar)
}
