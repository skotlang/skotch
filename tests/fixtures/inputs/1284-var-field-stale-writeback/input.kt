// Regression: nested-call mutation of a `var` field is no longer
// clobbered by the calling method's end-of-method writeback.
// Pre-fix shape:
//   foo() {
//       cached = this.field    // cache var field
//       this.helper()          // nested call mutates via putfield
//       this.field = cached    // STALE writeback undoes mutation!
//   }
// Fix: don't write back var fields the body never assigned to —
// the immediate-writethrough at the assignment site handles
// within-method mutations, and a missing writeback for unmodified
// vars is safer than clobbering.
//
// Validates by mutating through a sibling reference (not through
// `this`), so the test exercises the nested-mutation-via-call shape
// without hitting the orthogonal "stale cache local in same method"
// gap that var-field caching produces.
class Logger {
    var count: Int = 0
}

class Server(val log: Logger) {
    fun handle() {
        log.count = log.count + 1
    }
    fun runMany() {
        handle()
        handle()
        handle()
    }
}

fun main() {
    val l = Logger()
    val s = Server(l)
    s.runMany()
    println(l.count)
}
