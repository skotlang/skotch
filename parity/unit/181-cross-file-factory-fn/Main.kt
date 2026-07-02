// Isolates the "unresolved reference 'Ok'" / "'Err'" errors from
// parity/full/102-result — top-level generic factory functions live
// in a companion file/package and Main.kt imports them by simple
// name. skotch's resolver must find these across files and dispatch
// each call through the correct erased static method.
import foo.Wrap
import foo.Bare
import foo.WithArg

fun main() {
    println(Wrap(7))
    println(Wrap("hi"))
    println(Bare<Int>())
    println(WithArg(42, "num"))
    println(WithArg("x", "str"))
}
