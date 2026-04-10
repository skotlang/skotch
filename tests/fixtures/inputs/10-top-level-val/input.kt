// TODO (PR #1.5): top-level vals lower to a static final field plus a
// <clinit> method that initializes it. Not in PR #1 scope.
val GREETING = "hi"

fun main() {
    println(GREETING)
}
