// `inline fun <reified T>` — the type parameter is preserved at
// runtime through inlining. The body can use `T::class`, `T::class.java`,
// and `is T`. Standard idiom for Java-interop bridges and JSON
// deserializers.
//
// Skotch status (suspected): inline-fn body substitution landed (#362)
// for non-reified inline calls. Reified expansion likely doesn't
// substitute the type parameter — `is T` collapses to `is Any`.

inline fun <reified T> Any.isInstanceOf(): Boolean {
    return this is T
}

inline fun <reified T> firstOfType(items: List<Any>): T? {
    for (it in items) {
        if (it is T) return it
    }
    return null
}
