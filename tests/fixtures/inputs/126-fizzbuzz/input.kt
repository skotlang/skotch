fun fizzbuzz(n: Int): String {
    if (n % 15 == 0) {
        return "FizzBuzz"
    }
    if (n % 3 == 0) {
        return "Fizz"
    }
    if (n % 5 == 0) {
        return "Buzz"
    }
    return "other"
}

fun main() {
    println(fizzbuzz(3))
    println(fizzbuzz(5))
    println(fizzbuzz(15))
    println(fizzbuzz(7))
}
