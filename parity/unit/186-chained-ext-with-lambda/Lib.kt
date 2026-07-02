package foo

class Pipe(val value: Int)

// Chained extension fns each taking a trailing lambda — mirrors
// kotlin-result's `parsePositive(...).andThen { ... }.andThen { ... }`
// idiom. Each `.andThen { ... }` returns a fresh Pipe so the next
// chain step is called on the result of the previous.
inline fun Pipe.andThen(f: (Int) -> Pipe): Pipe = f(value)
inline fun Pipe.mapInt(f: (Int) -> Int): Pipe = Pipe(f(value))
inline fun Pipe.onValue(f: (Int) -> Unit): Pipe {
    f(value)
    return this
}
