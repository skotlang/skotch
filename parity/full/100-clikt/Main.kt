// Smoke test for parity/100-clikt — instantiates a tiny subset of the
// clikt public surface to verify it loaded under whichever compiler
// produced the lib classpath we're running against.
//
// Intentionally uses ONLY simple, stable clikt API surface so the
// example's pass/fail signal reflects "did the project's library
// classes load and resolve cleanly" rather than "did Main.kt happen
// to call a method that broke between clikt versions".

import com.github.ajalt.clikt.core.CliktError

fun main() {
    val err = CliktError("hello from clikt", statusCode = 42)
    println("class=${err::class.java.simpleName}")
    println("message=${err.message}")
    println("statusCode=${err.statusCode}")
    println("printError=${err.printError}")
}
