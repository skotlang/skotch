// Regression: a block-bodied function `fun foo() { expr() }` with no
// explicit `return` returns Unit, NOT the type of `expr`. Before the
// fix, `infer_body_return_ty` treated a trailing `Stmt::Expr` as the
// returned value and inferred `Ty::Any` for non-literal call
// expressions — `cart.add(...)` then emitted descriptors with
// `Ljava/lang/Object;` return where the real method returns void,
// failing with `NoSuchMethodError` at the call site.
class Bin {
    val items: MutableList<String> = mutableListOf()

    fun add(item: String) {
        items.add(item)
    }

    fun size(): Int = items.size
}

fun main() {
    val bin = Bin()
    bin.add("first")
    bin.add("second")
    println(bin.size())
}
