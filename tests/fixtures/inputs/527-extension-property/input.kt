val String.lastChar: Char
    get() = this[this.length - 1]

fun main() {
    println("Hello".lastChar)
}
