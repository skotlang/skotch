// Generic-return substitution + checkcast/unbox for user classes.
//
// Pre-fix: `Box<Int>.unwrap()` whose source signature was `fun
// unwrap(): T` lowered with dest_ty = `Ty::Any` (T erases to Object
// in MIR). Subsequent uses of the result — typically a `.method`
// call or arithmetic with another primitive — silently fell through
// to a null placeholder because the dest local's type didn't match
// the substituted T.
//
// Fix: when a method's MIR return type is `Ty::Any` AND the
// receiver is a `Ty::Class(C)` where C has type params AND the
// receiver has `local_generic_args`, emit a `CheckCast` (for class
// substitutions) or a `checkcast Boxed + invokevirtual valueOf`
// unbox sequence (for primitive substitutions) so the dest local
// is typed concretely.
//
// The fixture uses no explicit type annotation on `val ... =
// box.unwrap()` so skotch's typeck doesn't complain about the
// generic-erased return; the MIR-lower substitution still kicks
// in based on the receiver's `local_generic_args`.

class Box<T>(private val value: T) {
    fun unwrap(): T = value
}

fun useIntBox() {
    val intBox: Box<Int> = Box(42)
    val intVal = intBox.unwrap()
    println(intVal)
}

fun useStringBox() {
    val strBox: Box<String> = Box("hello")
    val strVal = strBox.unwrap()
    println(strVal)
}

fun main() {
    useIntBox()
    useStringBox()
}
