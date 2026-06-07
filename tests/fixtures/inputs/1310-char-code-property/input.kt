// `Char.code` (Kotlin stdlib's `inline val Char.code: Int`) lowering.
//
// Pre-fix: `ch.code` fell through the unresolved-field path and
// produced a `Const(Null)` placeholder. Stored into an `IntArray`
// slot, this came out as `aconst_null; astore N; aload tape; iload
// idx; aload N; aastore` — the verifier then rejected the aastore
// (`Type '[I' is not assignable to reference type`). For `Char` not
// stored into an array slot the null bubbled up as a 0-result in
// arithmetic, silently corrupting downstream values.
//
// Fix: in `Expr::Field` lowering, when `field_name == "code"` and the
// receiver peeks as `Ty::Char`, synthesize a `recv.toInt()` call —
// kotlinc's own desugaring of `inline val Char.code: Int get() = this.toInt()`.
// Also extended `lower_expr_preview_ty` to peek through `Expr::Index`
// (returning Char for `String[i]`, Int for `IntArray[i]`, etc.) so
// chains like `input[k].code` see the receiver as Char.

fun main() {
    val ch = 'A'
    println(ch.code)              // 65

    val s = "Hello"
    println(s[0].code)            // 72  — exercises the Index-peek path
    println(s[4].code)            // 111

    // Storing .code into an IntArray slot triggered the original
    // VerifyError before the fix.
    val arr = IntArray(3)
    arr[0] = 'a'.code             // 97
    arr[1] = s[1].code            // 101 (= 'e')
    arr[2] = ch.code              // 65
    println(arr[0])
    println(arr[1])
    println(arr[2])
}
