// Driver — exercises:
//   (a) inline fun <reified T> ... is T
//   (b) class-member-reference: String::length → (String) -> Int
//   (c) bound-instance-reference: counter::inc → () -> Unit

fun main() {
    // (b) Class-member reference
    val words = listOf("a", "ab", "abc")
    val lens = mapAll(words, String::length)
    println("lens=$lens")

    // (c) Bound-instance reference
    val c = Counter(0)
    applyTimes(5, c::inc)
    println("counter=${c.value()}")

    // (a) Reified — tested last because the gap throws
    val mixed: List<Any> = listOf(1, "hello", 3.14, "world", 42)
    val firstStr = firstOfType<String>(mixed)
    val firstInt = firstOfType<Int>(mixed)
    println("firstStr=$firstStr")
    println("firstInt=$firstInt")
    println("3.isInstanceOf<Int>=${(3 as Any).isInstanceOf<Int>()}")
    println("3.isInstanceOf<String>=${(3 as Any).isInstanceOf<String>()}")
}
