// A small arithmetic expression interpreter. Each Expr subclass holds
// references back to Expr (recursive self-typed fields), so eval() and
// show() recurse through the tree. Sealed class + abstract methods +
// virtual dispatch + self-recursion in one place.
sealed class Expr {
    abstract fun eval(): Int
    abstract fun show(): String
}

class Num(val n: Int) : Expr() {
    override fun eval(): Int = n
    override fun show(): String = "$n"
}

class Add(val l: Expr, val r: Expr) : Expr() {
    override fun eval(): Int = l.eval() + r.eval()
    override fun show(): String = "(${l.show()} + ${r.show()})"
}

class Mul(val l: Expr, val r: Expr) : Expr() {
    override fun eval(): Int = l.eval() * r.eval()
    override fun show(): String = "(${l.show()} * ${r.show()})"
}

class Neg(val x: Expr) : Expr() {
    override fun eval(): Int = -x.eval()
    override fun show(): String = "(-${x.show()})"
}
