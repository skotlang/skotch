val String.shout: String
    get() = this + "!"

fun main() {
    println("hello".shout)
}
