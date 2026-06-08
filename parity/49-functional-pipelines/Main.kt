// Drives all three modules. EACH section is deliberately written
// to exercise a known v0.50 gap WITHOUT a workaround — the goal is
// to surface the failure mode concretely, not to dodge it.

fun main() {
    // ─── GAP 1: tailrec TCO — deep recursion (100_000) ──────────
    // kotlinc emits goto for the self-tail-call → constant stack.
    // skotch parses `tailrec` but doesn't emit the goto → stack
    // overflow on inputs deeper than ~10_000.
    println(sumTo(100_000))            // expected: 5000050000
    println(fact(20))                   // expected: 2432902008176640000
    println(gcd(1_234_567_890L, 987_654_321L))  // expected: 9

    // ─── GAP 2: var-capture in inline lambda ────────────────────
    // `countWhere` increments `n` inside the lambda. Without
    // Ref-boxing, each closure invocation sees its own copy of n
    // and the running total is lost.
    val nums = listOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
    println(nums.countWhere { it % 2 == 0 })  // expected: 5

    // ─── GAP 3: foldMap with non-trivial reducer ────────────────
    // Inline + lambda + reduction. Exercises the same machinery
    // as fold + standard reduce patterns.
    val total = nums.foldMap(0) { acc, x -> acc + x }
    println(total)                      // expected: 55

    // ─── GAP 4: Elvis-then-return + val-aliasing in Queue ───────
    // dequeue() uses `val h = head ?: return null` (Elvis-then-
    // return is a known parser gap) AND the
    // `val r = h.value; head = h.next; return r` val-aliasing
    // pattern.
    val q = Queue<String>()
    q.enqueue("alpha")
    q.enqueue("beta")
    q.enqueue("gamma")
    println(q.size)                     // 3
    println(q.dequeue())                // "alpha"
    println(q.dequeue())                // "beta"
    println(q.size)                     // 1
    println(q.dequeue())                // "gamma"
    println(q.dequeue())                // null
    println(q.size)                     // 0

    // ─── GAP 5: inline reified across files ──────────────────────
    // Iterable<*>.firstOfType<T>() with reified T. Pipeline.kt
    // declares the inline fn; Main.kt uses it with concrete
    // type arg. Cross-file inline + reified.
    val mixed: List<Any> = listOf(1, "hello", 2.5, true, 100)
    val firstInt: Int? = mixed.firstOfType()
    val firstStr: String? = mixed.firstOfType()
    val firstBool: Boolean? = mixed.firstOfType()
    println(firstInt)                   // 1
    println(firstStr)                   // hello
    println(firstBool)                  // true
}
