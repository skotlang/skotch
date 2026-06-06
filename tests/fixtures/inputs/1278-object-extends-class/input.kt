// Regression for audit finding #18: ObjectDecl gained `parent_class`.
// Kotlin allows `object Foo : Parent(args)`, but the parser used to
// silently drop the parent class. Now the supertype helper handles
// `Parent(args)?, Iface1, Iface2` uniformly for class/object/enum/
// interface declarations.
//
// Locked-in behavior:
//   1. The object IS-A the parent at the type level — `val g: Greeter
//      = DefaultGreeter` typechecks.
//   2. The object's `<init>` calls the parent's `<init>(args)V`.
//   3. The JVM emits the parent as the class's super_class slot.
//   4. Calling a method INHERITED from the parent dispatches up the
//      chain (was broken pre-fix: returned null because the object
//      method-dispatcher only looked at the object's own methods).
abstract class Greeter(val tag: String) {
    abstract fun greet(): String
    fun label(): String = "[$tag]"
}

object DefaultGreeter : Greeter("default") {
    override fun greet(): String = "hello, world"
}

fun main() {
    println(DefaultGreeter.greet())
    // Inherited method dispatch — was returning null before the fix.
    println(DefaultGreeter.label())
    val g: Greeter = DefaultGreeter
    println(g.greet())
    println(g.label())
}
