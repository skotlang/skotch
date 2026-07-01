// Drives the IntSet user-class `operator fun contains` probe.
// IntSet's name contains the "Set" substring that the mir-lower
// intrinsic at ~9902 matched against — without the user-class
// guard added in this iteration, `2 in s` would mis-route to
// java/util/Set.contains and crash with IncompatibleClassChangeError.

fun main() {
    val s = IntSet()
    s.add(1)
    s.add(2)
    s.add(3)
    s.add(2)                          // dedup — `2 in this` is true
    s.add(7)
    println(2 in s)                   // true
    println(5 in s)                   // false
    println(7 in s)                   // true
    println(2 !in s)                  // false
    println(5 !in s)                  // true
    println(s.size())                 // 4

    // Empty set — exercises the early-return path.
    val empty = IntSet()
    println(1 in empty)               // false
    println(1 !in empty)              // true
    println(empty.size())             // 0

    // Sequential adds + remove (no remove method, just track size).
    val tally = IntSet()
    var i = 0
    while (i < 10) {
        tally.add(i)
        i = i + 1
    }
    println(tally.size())             // 10
    println(0 in tally)               // true
    println(9 in tally)               // true
    println(10 in tally)              // false
}
