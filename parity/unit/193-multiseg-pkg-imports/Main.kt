// Isolates 102-result's Main.kt failure mode: importing top-level
// factory fns, the generic value class itself, AND extension fns by
// simple name from a MULTI-SEGMENT package whose declaring files pull
// opt-in annotations from a nested subpackage. On 102-result every one
// of these imports reports "unresolved reference", and every
// `Result<V, E>` type annotation reports "no type arguments expected
// for 'class Result'".
@file:OptIn(
    com.example.result.annotation.UnsafeBoxValueAccess::class,
    com.example.result.annotation.UnsafeBoxErrorAccess::class,
)

import com.example.result.Box
import com.example.result.Err
import com.example.result.Ok
import com.example.result.getErrorOr
import com.example.result.getOr

fun main() {
    val ok: Box<Int, String> = Ok(7)
    val err: Box<Int, String> = Err("boom")
    println(ok.isOk)
    println(err.isErr)
    println(ok.getOr(0))
    println(err.getOr(-1))
    println(err.getErrorOr("none"))
    println(ok.value)
}
