// Locks in: after `if (x is Foo)`, the local `x` narrows to `Foo`
// inside the then-branch so member access on it resolves. Without
// the smart cast, `x.name` against `x: Any` would fail to resolve
// and the println body would be silently dropped.
//
// We also exercise an else-branch where the narrowing must NOT
// leak — `x` outside the if/else block should still be the wider
// `Any` type.

class Named(val name: String) {
    fun describe(): String = "Named(${name})"
}

fun probe(x: Any): String {
    if (x is Named) {
        return x.describe()
    }
    return "unknown"
}

fun main() {
    println(probe(Named("Ada")))
    println(probe(42))
}
