// Verifier-correctness regression test: `Long? ?: Long` in suspend
// context must emit a properly-typed elvis that doesn't crash the
// JVM verifier. Before the fix, `safe` emitted bytecode where the
// non-null branch left an `Long` on the stack but the `Boxing.boxLong`
// invokestatic at the merge expected a `long` primitive, causing an
// "inconsistent stackmap frames" VerifyError.
suspend fun safe(x: Long?): Long = x ?: 0L
