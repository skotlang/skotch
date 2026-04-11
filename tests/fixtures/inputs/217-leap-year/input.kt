fun isLeapYear(year: Int): Boolean {
    if (year % 400 == 0) { return true }
    if (year % 100 == 0) { return false }
    return year % 4 == 0
}

fun main() {
    println(isLeapYear(2000))
    println(isLeapYear(1900))
    println(isLeapYear(2024))
    println(isLeapYear(2023))
}
