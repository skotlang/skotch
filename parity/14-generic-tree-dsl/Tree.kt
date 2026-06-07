// A generic recursive tree built with a lambda-with-receiver DSL.
// Each node carries a typed value and a hand-rolled singly-linked
// list of children (using ArrayList would add a stdlib-intrinsic
// dependency that's orthogonal to the DSL exercise here).
//
// Sophistication beyond example 13:
//   - Generic class `Tree<T>` with one type parameter.
//   - Lambda-with-receiver (`Tree<T>.() -> Unit`) drives the DSL.
//   - Companion `of(...)` threads the receiver through the builder
//     block — exercises scope-function lowering.
//   - Recursive walk with a callback that captures visit depth.
class TreeNode<T>(val value: T, var next: TreeNode<T>?, val first: TreeNode<T>?)

class Tree<T>(val value: T) {
    // `firstChild` points at the first child; subsequent siblings are
    // chained via `next`. `lastChild` accelerates append.
    private var firstChild: Tree<T>? = null
    private var lastChild: Tree<T>? = null
    private var sibling: Tree<T>? = null

    fun child(label: T, init: Tree<T>.() -> Unit): Tree<T> {
        val node = Tree(label)
        node.init()
        appendChild(node)
        return node
    }

    fun leaf(label: T): Tree<T> {
        val node = Tree(label)
        appendChild(node)
        return node
    }

    private fun appendChild(node: Tree<T>) {
        val tail = lastChild
        if (tail == null) {
            firstChild = node
        } else {
            tail.sibling = node
        }
        lastChild = node
    }

    fun walk(visit: (T, Int) -> Unit, depth: Int) {
        visit(value, depth)
        var c = firstChild
        while (c != null) {
            c.walk(visit, depth + 1)
            c = c.sibling
        }
    }

    fun walk(visit: (T, Int) -> Unit) {
        walk(visit, 0)
    }

    companion object {
        fun <T> of(root: T, init: Tree<T>.() -> Unit): Tree<T> {
            val t = Tree(root)
            t.init()
            return t
        }
    }
}
