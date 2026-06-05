// Regression: stdlib numeric reductions on `List<Int>` /
// `Iterable<Int>` must dispatch to `CollectionsKt.sumOfInt`,
// `CollectionsKt.averageOfInt`, and `CollectionsKt.maxOrThrow` rather
// than non-existent `java.util.List.sum()V` / `.average()V`.
fun main() {
    val nums = listOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
    println("sum     = ${nums.sum()}")
    println("average = ${nums.average()}")
    println("max     = ${nums.max()}")
}
