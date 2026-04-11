fun clamp(value: Int, min: Int, max: Int): Int {
    if (value < min) {
        return min
    }
    if (value > max) {
        return max
    }
    return value
}

fun main() {
    println(clamp(5, 1, 10))
    println(clamp(-3, 0, 100))
    println(clamp(999, 0, 100))
}
