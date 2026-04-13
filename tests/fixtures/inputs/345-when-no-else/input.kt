sealed class S
class A : S()
class B : S()

fun f(s: S): String = when (s) {
    is A -> "a"
    is B -> "b"
}

fun main() {
    println(f(A()))
    println(f(B()))
}
