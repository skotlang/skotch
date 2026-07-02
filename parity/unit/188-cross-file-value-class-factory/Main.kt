// Isolates the exact 102-result Main.kt failure — cross-file
// top-level factory functions declared in another file/package,
// each returning a `@JvmInline value class Box<V, E>` value.
// Main.kt imports the factories by simple name and stores their
// results in a typed local.
//
// This mirrors kotlin-result's `Ok(value)` / `Err(error)` calls that
// Main.kt hits at every line 31, 35, 36, 55. When these fail to
// resolve or emit wrong bytecode, every downstream call
// (`.isOk`, `.getOr`, `.map { }`, `.andThen { }`) is unreachable.
import foo.Box
import foo.Ok
import foo.Err

fun main() {
    val ok: Box<Int, String> = Ok(7)
    val err: Box<Int, String> = Err("boom")
    println(ok.isOk)
    println(err.isOk)
}
