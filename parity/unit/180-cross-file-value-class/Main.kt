// Isolates the "no type arguments expected for 'class Result'" error
// from parity/full/102-result — when Main.kt writes
// `val r: Result<Int, String> = …` and `Result` is an @JvmInline value
// class with two type parameters declared in another file/package,
// skotch's typeck rejects the type application. Presumably it's
// finding kotlin.Result (a single-type-param stdlib value class) via
// auto-import and refusing the 2-arg annotation. Explicit import
// should override the auto-import.
import foo.Result

fun main() {
    val r: Result<Int, String> = Result(42)
    println(r.isOk)
}
