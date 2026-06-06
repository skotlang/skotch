// Regression for audit finding #18: ObjectDecl gained `parent_class`.
// Kotlin allows `object Foo : Parent(args)`, but the parser used to
// silently drop the parent class. Now the supertype helper handles
// `Parent(args)?, Iface1, Iface2` uniformly for class/object/enum/
// interface declarations.
//
// Inherited-method dispatch through the parent class is a follow-up
// — this fixture only locks in the parser + typeck + mir-lower
// supertype recognition: the object IS-A the parent at the type
// level, the parent's `<init>(args)V` is called from the object's
// `<init>`, and the JVM emits the parent as the class's super_class.
abstract class Greeter(val tag: String) {
    abstract fun greet(): String
}

object DefaultGreeter : Greeter("default") {
    override fun greet(): String = "hello, world"
}

fun main() {
    println(DefaultGreeter.greet())
    // The object IS-A Greeter at the type level — assignment must
    // typecheck without a "type mismatch" error.
    val g: Greeter = DefaultGreeter
    println(g.greet())
}
