annotation class MyTag

@MyTag
fun greet(): String = "Hello!"

fun main() {
    println(greet())
}
