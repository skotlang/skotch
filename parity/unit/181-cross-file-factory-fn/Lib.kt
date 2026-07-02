package foo

// A top-level generic function declared in another file/package,
// mirroring `fun <V> Ok(value: V): Result<V, Nothing>` in the
// kotlin-result library. Callers in Main.kt import it by simple name.
fun <V> Wrap(value: V): String = "Wrap($value)"
fun <V> Bare(): String = "Bare"
fun <V> WithArg(value: V, kind: String): String = "$kind($value)"
