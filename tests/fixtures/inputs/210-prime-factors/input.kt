fun main() {
    var n = 360
    var d = 2
    while (n > 1) {
        while (n % d == 0) {
            println(d)
            n /= d
        }
        d += 1
    }
}
