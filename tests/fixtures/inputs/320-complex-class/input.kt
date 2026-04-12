class Stack {
    var items: Int = 0
    fun push() { items++ }
    fun pop() { items-- }
    fun size(): Int = items
}

fun main() {
    val s = Stack()
    s.push()
    s.push()
    s.push()
    println(s.size())
    s.pop()
    println(s.size())
}
