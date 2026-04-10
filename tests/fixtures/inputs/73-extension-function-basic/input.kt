fun String.exclaim(): String = this + "!"

fun Int.isEven(): Boolean = this % 2 == 0

fun main() {
    println("hello".exclaim())
    println(4.isEven())
    println(7.isEven())
}
