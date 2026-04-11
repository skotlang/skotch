fun main() {
    for (i in 1..15) {
        val fizz = i % 3 == 0
        val buzz = i % 5 == 0
        if (fizz && buzz) {
            println("FizzBuzz")
        } else if (fizz) {
            println("Fizz")
        } else if (buzz) {
            println("Buzz")
        } else {
            println(i)
        }
    }
}
