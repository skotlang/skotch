// TODO: extension functions. Lowered to static methods taking the
// receiver as a leading parameter.
fun String.shout(): String = this + "!"

fun main() {
    println("hello".shout())
}
