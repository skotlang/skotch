// Regression: `list.isNotEmpty()` was dispatched as
// `invokeinterface java/util/List.isNotEmpty:()V` (void return) —
// the method doesn't exist on `java.util.List` and the descriptor
// itself is wrong. The next instruction (`ifeq` from
// `while (queue.isNotEmpty())`) then popped from an empty stack and
// the verifier rejected the body with "Operand stack underflow".
//
// Fix: add `isNotEmpty` and `isEmpty` and `size` to the List
// intrinsic dispatch table. `isNotEmpty` emits three MIR stmts:
// isEmpty() → empty; const false → zero; CmpEq(empty, zero) → result.
// `Bool` and `Int` share the JVM int slot, so `empty == false` is
// exactly `!empty`.
fun main() {
    val xs = mutableListOf<String>()
    println("empty? ${xs.isEmpty()}")
    println("not empty? ${xs.isNotEmpty()}")
    xs.add("hi")
    println("empty after add? ${xs.isEmpty()}")
    println("not empty after add? ${xs.isNotEmpty()}")
    println("size: ${xs.size}")
}
