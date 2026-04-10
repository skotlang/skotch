// TODO: generics. Need erasure-aware lowering and signature attribute emission.
fun <T> identity(x: T): T = x

fun main() {
    println(identity(7))
}
