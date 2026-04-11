fun binarySearch(target: Int, size: Int): Int {
    var lo = 0
    var hi = size - 1
    while (lo <= hi) {
        val mid = (lo + hi) / 2
        if (mid == target) {
            return mid
        }
        if (mid < target) {
            lo = mid + 1
        } else {
            hi = mid - 1
        }
    }
    return -1
}

fun main() {
    println(binarySearch(7, 20))
    println(binarySearch(0, 20))
    println(binarySearch(19, 20))
    println(binarySearch(25, 20))
}
