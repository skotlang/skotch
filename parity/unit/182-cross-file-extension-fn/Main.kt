// Isolates the "unresolved reference 'getOr'/'map'/'andThen'/..."
// errors from parity/full/102-result — extension functions declared
// in a companion file/package and imported by simple name into
// Main.kt. skotch's resolver must find each extension in the import
// map, match its receiver type, and dispatch through the mangled
// static forwarder.
import foo.Container
import foo.doubled
import foo.plus
import foo.describe

fun main() {
    val c = Container(7)
    println(c.doubled())
    println(c.plus(3))
    println(c.describe("value"))
}
