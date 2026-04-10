// TODO (PR #1.5): if-as-expression requires Branch terminators in MIR
// and a StackMapTable attribute on the JVM side.
fun main() {
    val x = if (true) 1 else 2
    println(x)
}
