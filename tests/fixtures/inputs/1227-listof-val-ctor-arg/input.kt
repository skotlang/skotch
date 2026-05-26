// A top-level `val xs = listOf(...)` must infer its field type as the
// collection interface (erased to java/util/List, java/util/Set, … on the
// JVM), not Object. Before this fix the inferred
// `Ty::Generic{kotlin/collections/List}` fell through to
// `Ljava/lang/Object;` in the field / getstatic / putstatic descriptors,
// so the static field was declared `Object` while its getter disagreed,
// and any List-typed use (e.g. passing it to a `List<T>` constructor
// parameter, as JetChat's FakeData → ConversationUiState does) failed the
// verifier. This locks the field descriptors to their Java erasure.
val items = listOf("x", "y", "z")
val tags = setOf(1, 2)

fun main() {
    println(items)
    println(tags)
}
