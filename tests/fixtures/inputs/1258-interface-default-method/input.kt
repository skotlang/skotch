// Regression: an interface with a default-bodied method (one that
// has an implementation) must compile to a JVM `default` method that
// concrete implementors can either inherit or override. Before the
// interface-constructor validator carve-out, the interface
// `<init>` placeholder (empty blocks, non-abstract) failed
// `validate_class_method` and the whole file errored out.
interface Describable {
    fun name(): String
    fun describe(): String = "[${name()}]"
}

class Box(val label: String) : Describable {
    override fun name(): String = label
}

class FancyBox(val label: String) : Describable {
    override fun name(): String = label
    override fun describe(): String = "*${label}*"
}

fun main() {
    val a: Describable = Box("a")
    val b: Describable = FancyBox("b")
    println(a.describe())
    println(b.describe())
}
