fun isAdult(age: Int): Boolean = age >= 18
fun isTeenager(age: Int): Boolean = age >= 13 && age < 18

fun main() {
    println(isAdult(20))
    println(isAdult(10))
    println(isTeenager(15))
    println(isTeenager(20))
}
