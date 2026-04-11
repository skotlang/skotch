fun Int.isPrime(): Boolean {
    if (this < 2) { return false }
    var i = 2
    while (i * i <= this) {
        if (this % i == 0) { return false }
        i += 1
    }
    return true
}

fun main() {
    var count = 0
    for (n in 2..100) {
        if (n.isPrime()) {
            count += 1
        }
    }
    println(count)
}
