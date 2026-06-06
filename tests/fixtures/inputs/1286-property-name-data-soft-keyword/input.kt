// Regression: `data` is a soft keyword in Kotlin — legal as an
// identifier in identifier positions like a property name. Previously
// the parser consumed `data` as a class-level modifier even inside
// a property declaration, leaving the PropertyDecl with an empty
// name; mir-lower then panicked at
//   `format!("get{}{}", &field.name[..1].to_uppercase(), …)`
// with "end byte index 1 is out of bounds for string of length 0".
//
// Fix: after seeing `val`/`var`, accept any soft keyword (data,
// open, init, infix, operator, …) as the property name. The fix
// surfaces ALL soft-keyword tokens via the new
// `TokenKind::is_soft_keyword()` helper.
class Holder {
    val data: Int = 7
    val operator: String = "+"
    val init: Int = 0
}

fun main() {
    val h = Holder()
    println(h.data)
    println(h.operator)
    println(h.init)
}
