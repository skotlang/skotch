// Regression for audit fix #5/#21: TypeDecl.enum_entries is populated
// cross-file from ExternalClassDecl.enum_entries. Verifies the
// when-exhaustiveness check fires correctly when the enum subject is
// declared in the same file (the cross-file path is exercised by
// xtask's multi-file projects but a single-file fixture is sufficient
// to lock in the gather→typeck data flow now that the resolver
// populates `enum_entries` uniformly).
enum class Channel { ALPHA, BETA, GAMMA }

fun describe(c: Channel): String = when (c) {
    Channel.ALPHA -> "alpha"
    Channel.BETA -> "beta"
    Channel.GAMMA -> "gamma"
}

fun main() {
    println(describe(Channel.ALPHA))
    println(describe(Channel.BETA))
    println(describe(Channel.GAMMA))
}
