// Mathematical helpers that exercise deep recursion (requires
// tailrec TCO — known v0.50 gap) and infix operations. Deliberately
// pushes recursion depth to 100_000 to ensure a non-TCO compile
// blows the JVM stack.

tailrec fun sumTo(n: Int, acc: Long = 0L): Long {
    if (n == 0) return acc
    return sumTo(n - 1, acc + n)
}

tailrec fun fact(n: Int, acc: Long = 1L): Long {
    if (n <= 1) return acc
    return fact(n - 1, acc * n)
}

tailrec fun gcd(a: Long, b: Long): Long {
    if (b == 0L) return a
    return gcd(b, a % b)
}
