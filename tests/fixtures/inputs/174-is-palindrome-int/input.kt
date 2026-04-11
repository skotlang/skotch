fun isPalindrome(n: Int): Boolean {
    if (n < 0) {
        return false
    }
    var original = n
    var reversed = 0
    while (original > 0) {
        reversed = reversed * 10 + original % 10
        original = original / 10
    }
    return reversed == n
}

fun main() {
    println(isPalindrome(121))
    println(isPalindrome(123))
    println(isPalindrome(1221))
    println(isPalindrome(-121))
}
