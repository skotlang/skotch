typealias Name = String
typealias Age = Int

fun greet(n: Name, a: Age): String = "Hello, $n (age $a)"

fun main() {
    val name: Name = "Alice"
    val age: Age = 30
    println(greet(name, age))
}
