// Try-finally with a cleanup that pushes onto the operand stack
// (`println("cleanup")` = ldc + getstatic = stack 2). Pre-fix the
// max_stack dataflow recomputation at recompute_max_stack_from_code
// only walked successors from offset 0 (and goto/branch targets) —
// exception-handler entry offsets were never reached, so the
// handler block's stack contribution was missed. max_stack stayed
// at the try body's peak (often 1) → `VerifyError: Operand stack
// overflow` when the cleanup pushed 2 items.
//
// Fix: new `recompute_max_stack_from_code_with_handlers` variant
// seeds handler_starts with `depth_in[h] = Some(1)` (the JVM pushes
// the thrown exception onto the stack at handler entry) and adds
// them to the work queue. All three recompute callers in the main
// JVM emit path updated to pass `func.exception_handlers` mapped
// through `block_offsets`. Surfaced by parity/44-exception-handling
// while probing try-catch-finally.
//
// NOTE: separate gap NOT fixed in this iteration — `try { return X }
// finally { cleanup }` skips the finally on early-return.

fun runWithCleanup(): Int {
    var result = 0
    try {
        result = 42
    } finally {
        println("cleanup")
        println("done")
    }
    return result
}

fun main() {
    println(runWithCleanup())
}
