fun ageGroup(age: Int): String = when (age) {
    in 0..12 -> "child"
    in 13..17 -> "teenager"
    in 18..64 -> "adult"
    in 65..120 -> "senior"
    else -> "invalid"
}

fun main() {
    println(ageGroup(5))
    println(ageGroup(15))
    println(ageGroup(30))
    println(ageGroup(70))
    println(ageGroup(-1))
}
