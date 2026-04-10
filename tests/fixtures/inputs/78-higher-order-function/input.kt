fun <T> List<T>.myForEach(action: (T) -> Unit) {
    for (item in this) {
        action(item)
    }
}

fun main() {
    listOf("a", "b", "c").myForEach { println(it) }
}
