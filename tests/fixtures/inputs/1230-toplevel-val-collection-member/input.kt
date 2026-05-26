// A top-level `val` of collection type is typed `Ty::Generic { base: List }`
// by `infer_top_level_val_ty`, whereas a *local* initialized from `listOf(...)`
// is typed `Ty::Class` by the call-lowering path. Member resolution in
// mir-lower keys on `Ty::Class`, so a property access on the top-level val —
// `xs.size` here — must first unwrap the generic to its base class. Before the
// fix it failed to lower, and because a single failed statement aborts the
// whole function body, `main` came out empty (no output at all). Mirrors how a
// JetChat composable reads `.size` off a module-level collection.
val xs = listOf("a", "b", "c")

fun main() {
    println(xs.size)
}
