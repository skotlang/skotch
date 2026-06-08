// Method dispatched via implicit `this` inside a lambda-with-receiver
// body picked the WRONG MirClass when the JVM backend resolved the
// invokevirtual descriptor. `module.classes` carries two entries for
// the same class name in a single-file program:
//
//   1. The cross-file STUB (registered for forward-declaration support);
//      its `methods[]` retains the post-Pass-1 placeholder shape with
//      `return_ty = Ty::Any`.
//   2. The real lowered MirClass with proper Ty::Unit/etc. return types.
//
// At class_writer.rs:~16358 the backend looked up
// `module.classes.iter().find(|c| c.name == "HTML")` — first-match — and
// hit the stub. The descriptor it emitted was
// `invokevirtual HTML.foo:()Ljava/lang/Object;`, but the real method's
// signature is `()V`. Runtime: `NoSuchMethodError` on every implicit-this
// call inside a lambda-with-receiver body.
//
// Fix at class_writer.rs:~16358 (and the matching pre_target_sig
// lookup at ~16224) switches to `module.find_class()`, which already
// prefers the non-stub MirClass when both exist.

class HTML {
    fun foo() = println("HTML.foo")
}

fun html(init: HTML.() -> Unit) {
    val h = HTML()
    h.init()
}

fun main() {
    html {
        foo()
    }
}
