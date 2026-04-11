fun printTimes(msg: String, count: Int) {
    for (i in 1..count) {
        println(msg)
    }
}

fun main() {
    printTimes("hello", 3)
    printTimes("world", 2)
}
