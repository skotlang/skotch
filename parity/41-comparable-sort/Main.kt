// Drives both Comparable impls. The two `sorted()` calls exercise
// the synthesized `compareTo(Object)` bridge — `kotlin.collections
// .CollectionsKt.sorted` ultimately dispatches through
// `((Comparable)o).compareTo(other)`, so without the bridge,
// every call site throws `AbstractMethodError` at runtime.

fun main() {
    val people = listOf(
        Person("Carol", 35),
        Person("Alice", 30),
        Person("Bob", 25)
    )
    for (p in people.sorted()) {
        println(p)
    }
    println("---")

    val items = listOf(
        Item("low", 1),
        Item("high", 9),
        Item("mid", 5)
    )
    for (i in items.sorted()) {
        println(i)
    }
    println("---")

    // Sort a single-element list — no comparisons happen, but the
    // type machinery still flows through the bridge.
    for (p in listOf(Person("Dave", 40)).sorted()) {
        println(p)
    }
    println("---")

    // Sort by direct comparison (no list) — `Person.compareTo` on
    // its own works without the bridge (uses the typed method).
    val a = Person("Eve", 22)
    val b = Person("Frank", 28)
    println(a.compareTo(b))
}
