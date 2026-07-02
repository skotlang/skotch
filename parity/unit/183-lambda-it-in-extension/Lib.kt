package foo

class Container(val value: String)

// Extension function that takes a lambda parameter — mirrors
// `Result<V,E>.mapError { transform }` in the kotlin-result library.
inline fun Container.mapValue(transform: (String) -> String): Container =
    Container(transform(value))

inline fun <T> Container.foldValue(init: T, step: (T, String) -> T): T =
    step(init, value)
