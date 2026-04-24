fun main() {
    val greeting: String by lazy { "Hello, World!" }
    println(greeting)
    val number: Int by lazy { 42 }
    println(number)
}
