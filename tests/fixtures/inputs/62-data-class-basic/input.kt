data class User(val name: String, val age: Int)

fun main() {
    val u = User("Alice", 30)
    println(u)
    println(u.name)
    println(u.age)
}
