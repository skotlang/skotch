// Isolates the CROSS-FILE variant of fixture 189: extension fns on a
// generic @JvmInline value class where the extension is declared in a
// different file (Ext.kt) than the value class (Lib.kt).
//
// Fixture 189 (single-file) was fixed, yet all 51 of 102-result's
// corresponding library fns kept bailing — because in kotlin-result the
// class (Result.kt) and its extensions (And.kt, Get.kt, Unwrap.kt, ...)
// are in separate files. Minimal bisect confirmed: compiling real
// Result.kt + Unwrap.kt alone still bails `unwrap` with Return(When)
// and `expect` with Call(Reference, Lambda).
import foo.Err
import foo.Ok
import foo.Box
import foo.thenMap
import foo.unwrapOr

fun main() {
    val ok: Box<Int, String> = Ok(7)
    val none: Box<Int, String> = Err("nope")
    println(ok.unwrapOr(0))
    println(none.unwrapOr(-1))
    val mapped = ok.thenMap { v -> Ok(v * 2) }
    val skipped = none.thenMap { v -> Ok(v) }
    println(mapped.value)
    println(mapped.isOk)
    println(skipped.isErr)
}
