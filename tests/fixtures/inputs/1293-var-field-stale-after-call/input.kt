// Regression: a method pre-loaded `this.pos` into a method-local
// cache at function entry. After a nested call (`expect('"')` here)
// mutated `this.pos` via `putfield`, subsequent reads of the cache
// still saw the pre-mutation value. The string `"hi"` got parsed as
// "" because the read loop started from the OPENING `"` instead of
// after it.
//
// Fix: mir-lower post-pass that, after every Call MIR statement,
// re-loads `field_local = GetField(this, field)` for every
// var-field cached in `field_local_writebacks`.
class P(val src: String) {
    var pos: Int = 0

    fun expect(c: Char) {
        if (src[pos] != c) throw IllegalStateException("nope")
        pos++
    }

    fun parseStringLit(): String {
        expect('"')
        val sb = StringBuilder()
        while (pos < src.length) {
            val c = src[pos]
            if (c == '"') {
                pos++
                return sb.toString()
            }
            sb.append(c)
            pos++
        }
        return sb.toString()
    }
}

fun main() {
    val p = P("\"hi\"")
    println(p.parseStringLit())
}
