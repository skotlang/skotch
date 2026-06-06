// Regression: a HOF taking a `(A, B) -> R` lambda where A and B are
// non-primitive must propagate BOTH declared param types into the
// lambda body so member access on the lambda's params resolves to
// concrete classes (not Ty::Any → Const(null)). Before the fix,
// `lambda_param_types` was only set for the fold-family; other
// multi-arg HOFs erased the lambda params to Object.
interface Tag {
    fun label(): String
}

class TagA : Tag {
    override fun label(): String = "A"
}

class TagB : Tag {
    override fun label(): String = "B"
}

fun pair(t1: Tag, t2: Tag, fmt: (Tag, Tag) -> String): String = fmt(t1, t2)

fun main() {
    val result = pair(TagA(), TagB()) { a, b -> "${a.label()}+${b.label()}" }
    println(result)
}
