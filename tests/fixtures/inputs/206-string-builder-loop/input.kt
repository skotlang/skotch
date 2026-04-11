fun joinNumbers(n: Int): String {
    var result = ""
    for (i in 1..n) {
        if (i > 1) {
            result = result + ", "
        }
        result = result + "$i"
    }
    return result
}

fun main() {
    println(joinNumbers(5))
    println(joinNumbers(1))
}
