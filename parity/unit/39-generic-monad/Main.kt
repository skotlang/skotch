// Drives all three files:
//   - Box.kt: generic Box<A,B>.swap()/firstOnly()/secondOnly()
//     + cross-file fns `pair` and `firstOf`.
//   - Utility.kt: `triplets()` and `Counter` (user-class methods
//     named like stdlib extensions — proves user methods win).

fun main() {
    // Generic class with two type params + swap (A↔B flip).
    // Reference-only types — cross-file generic fn with primitive
    // args hits a separate autobox gap; see Utility.kt note.
    val p = pair("hello", "world")
    println(p)                       // (hello, world)
    println(p.swap())                // (world, hello)
    println(p.firstOnly())           // hello
    println(p.secondOnly())          // world

    // Cross-file generic fn `firstOf` on the same Box.
    println(firstOf(p))              // hello

    // Mixed-type Boxes from cross-file `triplets`.
    println(triplets())              // (x, 1) | (y, two) | (z, end)

    // Counter — methods NAMED like stdlib extensions (forEach, map).
    // If dispatch is broken, these'd route to CollectionsKt.forEach
    // (Iterable) and crash. Correct dispatch invokes the user method.
    val c = Counter(7)
    println(c.count())               // 7
    println(c.forEach())             // "iterated 7 times"
    println(c.map())                 // "mapped to 7"
}
