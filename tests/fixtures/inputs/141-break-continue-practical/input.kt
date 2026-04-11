fun findFirst(target: Int): Int {
    for (i in 1..100) {
        if (i * i > target) {
            return i
        }
    }
    return -1
}

fun main() {
    var sum = 0
    for (i in 1..100) {
        if (i % 3 != 0) {
            continue
        }
        sum += i
        if (sum > 50) {
            break
        }
    }
    println(sum)
    println(findFirst(50))
}
