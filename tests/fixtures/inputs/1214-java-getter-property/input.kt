import java.io.File

// Verifies Kotlin's property syntax resolves Java getter methods —
// `f.name` becomes `f.getName()` and `f.path` becomes `f.getPath()`.
//
// We use a filename without directory separators so the test output
// is identical on Unix and Windows. `File.getPath()` normalizes the
// separators to `/` vs `\` depending on the host OS, which a
// path-with-directories input would expose; that's not what this
// fixture is checking — it's checking Java-getter → Kotlin-property
// translation. A second `File` with a longer name (still no
// separator) keeps the test from collapsing to two identical lines.
fun main() {
    val f = File("skotch-1214")
    println(f.name)
    val g = File("skotch-1214.txt")
    println(g.name)
}
