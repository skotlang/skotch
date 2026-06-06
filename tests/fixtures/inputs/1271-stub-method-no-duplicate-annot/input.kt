// Regression: a stub method (`emit_method` short-circuits to
// `emit_stub_method` when `has_null_stubs_why` or
// `has_type_flow_issues` fires) must NOT itself append
// `RuntimeInvisibleAnnotations` / `RuntimeInvisibleParameterAnnotations`.
// The caller of `emit_method` does that on the returned blob, so a
// duplicate append produced TWO copies of each attribute and the JVM
// rejected the class file at load time with
// `ClassFormatError: Multiple RuntimeInvisibleAnnotations attributes for method`.
//
// This fixture intentionally hits the stub path by combining a
// non-null-checked reference return with a `when` whose first branch
// returns a sibling subtype — kotlinc still emits a real body, but
// pre-fix skotch fell to the stub emitter and crashed when the
// resulting class was loaded.
interface Tag {
    fun id(): Int
}

class A : Tag {
    override fun id(): Int = 1
}

class B : Tag {
    override fun id(): Int = 2
}

fun pick(b: Boolean): Tag = if (b) A() else B()

fun main() {
    println(pick(true).id())
    println(pick(false).id())
}
