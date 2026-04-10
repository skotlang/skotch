typealias StringList = List<String>
typealias Predicate<T> = (T) -> Boolean

fun filter(list: StringList, pred: Predicate<String>): StringList {
    return list.filter(pred)
}

fun main() {
    val result = filter(listOf("apple", "banana", "avocado")) { it.startsWith("a") }
    println(result)
}
