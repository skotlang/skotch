// Locks in: `list.map { it.field }` where `list` is `List<DataClass>`
// flows the element type into `it`, so member access compiles.

data class User(val name: String, val age: Int)

fun main() {
    val users = listOf(User("Ada", 36), User("Bob", 24), User("Cy", 51))
    val names = users.map { it.name }
    for (n in names) {
        println(n)
    }
}
