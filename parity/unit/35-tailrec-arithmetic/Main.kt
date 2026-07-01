// Drives every helper from Math.kt and Mod.kt.
//
// NOTE: this example would IDEALLY stress sumTo(100_000) — at that
// depth, a non-tail-recursive emission blows the default JVM stack
// with a StackOverflowError, while a goto-based tailrec compile
// finishes instantly. skotch currently parses `tailrec` but does
// NOT emit the goto rewrite (milestones v0.50 tracks the gap), so
// the depths below are capped at what fits on the default JVM
// stack with real recursive calls. When tailrec TCO lands, raise
// the cap and the example becomes a genuine TCO regression test.

fun main() {
    // Deep-recursion via tailrec accumulator. 2_000 is safely under
    // the default JVM stack limit even WITHOUT TCO; when skotch
    // grows real tailrec, bump this to 100_000.
    println(sumTo(2_000))        // 2001000
    println(sumTo(1_000))        // 500500
    println(sumTo(10))           // 55
    println(sumTo(0))            // 0 (default-acc path)

    // tailrec with four params + Long arithmetic + modular reduction.
    println(powMod(2L, 30, 1_000_000_007L))    // 73741817 (2^30 mod 1e9+7)
    println(powMod(7L, 50, 1_000_000L))        // 251249   (last 6 digits of 7^50)
    println(powMod(3L, 0, 17L))                // 1        (acc default)

    // tailrec shallow-recursion (log-depth) plus a non-tailrec
    // top-level fn that consumes it.
    println(gcd(123_456_789L, 987_654_321L))   // 9
    println(gcd(100L, 75L))                    // 25
    println(gcd(7L, 13L))                      // 1 (coprime)

    println(lcm(12L, 18L))                     // 36
    println(lcm(100L, 75L))                    // 300
    println(lcm(7L, 13L))                      // 91
}
