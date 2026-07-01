// `tailrec` accumulator pattern with a default-parameter seed.
//
// Kotlin's `tailrec` modifier asks the compiler to compile the
// terminal self-call as a `goto` back to the function entry instead
// of a real method invocation. Without that optimization, deep
// inputs blow the JVM call stack — with it, `sumTo(100_000)` runs
// in constant stack frames.
//
// `acc: Long = 0L` exercises a default parameter whose value is a
// typed numeric literal — call sites pass only `n`, and kotlinc
// synthesizes a `$default` overload that fills in 0L.

tailrec fun sumTo(n: Int, acc: Long = 0L): Long {
    if (n == 0) return acc
    return sumTo(n - 1, acc + n)
}

// Modular exponentiation, also tail-recursive. Combines a Long
// accumulator with an Int counter and a Long modulus. The body
// performs a Long × Long multiplication before taking the mod,
// so overflow inside the modulus is the user's responsibility.
tailrec fun powMod(base: Long, exp: Int, mod: Long, acc: Long = 1L): Long {
    if (exp == 0) return acc
    return powMod(base, exp - 1, mod, (acc * base) % mod)
}
