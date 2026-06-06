// Regression: a user class extending a stdlib exception like
// `RuntimeException` must resolve the super class to its FQ
// `java/lang/RuntimeException` for BOTH the class header's super_class
// slot AND the `<init>`-emitted methodref for the super-ctor call.
// Before the fix, the methodref used the short `RuntimeException`
// while the class header was correct — verifier rejected with
// "Type 'RuntimeException' is not assignable to 'CalcError'".
class CalcError(message: String) : RuntimeException(message)

fun main() {
    try {
        throw CalcError("boom")
    } catch (ex: CalcError) {
        println("caught: ${ex.message}")
    }
}
