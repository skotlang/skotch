// Lock-in for parity/101-hash work: an unqualified call to a parent
// class's companion-object method must resolve through `getstatic
// Parent.Companion` followed by `invokevirtual
// Parent$Companion.method`. Pre-fix the implicit-`this` BFS only
// walked the receiver class's own methods + interfaces + super
// methods; companion members went unseen and the call failed with
// "unknown call target" inside the secondary ctor's super
// delegation.
abstract class Parent {
    companion object {
        fun helper(n: Int): Int = n * 2
    }
}

class Child : Parent {
    constructor(x: Int) : super() {
        println(helper(x))
    }
}

fun main() {
    Child(7)
}
