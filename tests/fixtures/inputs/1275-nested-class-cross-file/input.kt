// Regression for audit finding #9: nested classes invisible cross-file.
// Previously `ClassDecl.nested_classes` was populated but never
// surfaced through `ExternalClassDecl`, so `Outer.Nested(...)` from
// another file silently dropped. Now `gather_class_recursive` walks
// every nested class and registers each as its own ExternalClassDecl
// with an `Outer$Nested` JVM name.
//
// This in-file fixture validates the parser+registration path; a
// follow-up will add a true two-file fixture once nested-class type
// resolution in the call site is wired through.
class Outer(val tag: String) {
    class Nested(val n: Int) {
        fun describe(): String = "nested[$n]"
    }
}

fun main() {
    val n = Outer.Nested(42)
    println(n.describe())
    val n2 = Outer.Nested(7)
    println(n2.describe())
    println(Outer("alpha").tag)
}
