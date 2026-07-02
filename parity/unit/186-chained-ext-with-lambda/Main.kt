// Isolates the chained-extension-with-lambda shape from
// parity/full/102-result — Main.kt has
//   parsePositive("12")
//     .andThen { parsePositive((it * 3).toString()) }
//     .andThen { Ok(it + 1) }
// each `.andThen { ... }` is a cross-file inline extension that
// returns a fresh Result on which the next `.andThen { ... }` is
// then called. skotch's resolver + mir-lower has to walk the
// dot-chain, resolve each extension across files, and dispatch each
// through its mangled static forwarder.
import foo.Pipe
import foo.andThen
import foo.mapInt
import foo.onValue

fun main() {
    val result = Pipe(7)
        .andThen { Pipe(it * 2) }
        .andThen { Pipe(it + 1) }
        .mapInt { it * 10 }
    println(result.value)

    val traced = Pipe(3)
        .onValue { println("first=$it") }
        .mapInt { it * 4 }
        .onValue { println("mid=$it") }
        .andThen { Pipe(-it) }
    println(traced.value)
}
