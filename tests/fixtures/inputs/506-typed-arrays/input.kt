fun main() {
    val bytes = ByteArray(3)
    bytes[0] = 1
    bytes[1] = 2
    bytes[2] = 3
    println(bytes.size)

    val doubles = DoubleArray(2)
    doubles[0] = 1.5
    doubles[1] = 2.5
    println(doubles[0] + doubles[1])
}
