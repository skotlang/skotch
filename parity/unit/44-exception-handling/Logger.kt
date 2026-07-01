// Simple state container — `Counter` tracks how many `try` blocks
// have run (via the `finally` branch incrementing on every exit)
// and what the last caught error was. Cross-file: Main.kt mutates
// this through `c.bump()` and `c.setError()`.

class Counter {
    var count: Int = 0
    var lastError: String = ""
    fun bump() {
        count = count + 1
    }
    fun setError(msg: String) {
        lastError = msg
    }
}
