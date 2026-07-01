// Euclid's GCD — naturally shallow recursion (O(log min(a,b))) but
// the textbook example of a tailrec function. Useful as the building
// block for `lcm`, which is NOT tailrec.

tailrec fun gcd(a: Long, b: Long): Long {
    if (b == 0L) return a
    return gcd(b, a % b)
}

// `lcm` deliberately divides BEFORE multiplying to avoid overflow on
// values whose product would exceed Long.MAX_VALUE but whose lcm
// still fits. Cross-file: this function calls into Math.kt's neighbor
// only indirectly (via the shared gcd above), confirming cross-file
// resolution of non-tailrec utilities sitting next to tailrec ones.
fun lcm(a: Long, b: Long): Long = a / gcd(a, b) * b
