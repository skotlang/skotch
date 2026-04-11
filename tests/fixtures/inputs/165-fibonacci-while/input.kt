fun main() {
    var a = 1
    var b = 1
    while (a + b < 100) {
        val temp = a + b
        a = b
        b = temp
    }
    println(b)
}
