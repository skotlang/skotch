// Smoke test for parity/102-result — exercises the kotlin-result
// public surface to verify the project's compiled classes load and
// dispatch correctly through whatever compiler produced them.
//
// Touches:
//   - top-level constructors `Ok(value)` / `Err(error)` (which return
//     Kotlin value-class `Result<V, E>` instances)
//   - the `getOr` / `getError` / `isOk` / `isErr` accessor surface
//   - `map`, `mapError`, `andThen` chained transformations
//   - `runCatching` (a contracts-using inline fn)
//
// Intentionally avoids the `binding {}` DSL because its
// `BindingException` is the expect/actual class that gives skotch's
// multiplatform handling the hardest time — failing on that path is
// the EXPECTED gap and the bench already reports it; we want Main.kt
// to surface a clean stdout when the project library is otherwise
// usable.

import com.github.michaelbull.result.Ok
import com.github.michaelbull.result.Err
import com.github.michaelbull.result.Result
import com.github.michaelbull.result.andThen
import com.github.michaelbull.result.getOr
import com.github.michaelbull.result.getError
import com.github.michaelbull.result.map
import com.github.michaelbull.result.mapError
import com.github.michaelbull.result.runCatching

private fun parsePositive(s: String): Result<Int, String> {
    val n = s.toIntOrNull() ?: return Err("not a number: $s")
    return if (n > 0) Ok(n) else Err("non-positive: $n")
}

fun main() {
    val ok: Result<Int, String> = Ok(7)
    val err: Result<Int, String> = Err("boom")

    // Accessors
    println("ok.isOk=${ok.isOk}")
    println("err.isErr=${err.isErr}")
    println("ok.getOr=-1=${ok.getOr(-1)}")
    println("err.getOr=-1=${err.getOr(-1)}")
    println("ok.getError=${ok.getError()}")
    println("err.getError=${err.getError()}")

    // Transformations
    val doubled = ok.map { it * 2 }
    val errUpper = err.mapError { it.uppercase() }
    println("doubled=${doubled.getOr(0)}")
    println("errUpper=${errUpper.getError()}")

    // Chained parse — pipeline of Result<Int,String> -> Result<Int,String>
    val parsed: Result<Int, String> = parsePositive("12")
        .andThen { parsePositive((it * 3).toString()) }
        .andThen { Ok(it + 1) }
    println("parsed=${parsed.getOr(-1)}")
    val badParsed = parsePositive("-5").andThen { Ok(it + 1) }
    println("badParsed=${badParsed.getError()}")

    // runCatching converts a throwing block into a Result<V, Throwable>
    val caught: Result<Int, Throwable> = runCatching { "42".toInt() }
    val crashed: Result<Int, Throwable> = runCatching { "nope".toInt() }
    println("caught.getOr=${caught.getOr(-1)}")
    println("crashed.getError.class=${crashed.getError()!!::class.java.simpleName}")
}
