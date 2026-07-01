// Mini JSON value tree + recursive-descent parser.
//
// Sophistication step over example 16:
//   - sealed class with FIVE data-carrying subclasses (Null is an
//     object, Bool/Num/Str carry a single value, Arr/Obj carry
//     polymorphic-element collections)
//   - recursive parser: parseValue → parseArray/Object → parseValue …
//   - character-by-character cursor (Int) walked through the source
//   - when (Char) dispatch on the current cursor position
//   - escape-sequence handling inside string literals
//   - signed integer parsing (no exposed Int.parseInt yet, so we
//     hand-roll it from char digits)
//
// Out of scope: floats, scientific notation, Unicode escapes — we want
// to exercise the parser shape, not be a spec-conforming JSON impl.

sealed class JsonValue

object JsonNull : JsonValue()

class JsonBool(val value: Boolean) : JsonValue()

class JsonNum(val value: Int) : JsonValue()

class JsonStr(val value: String) : JsonValue()

class JsonArr(val items: MutableList<JsonValue>) : JsonValue()

// Two parallel lists so iteration can pull key/value via plain
// `keys[i]` / `values[i]` indexing without depending on the still-
// in-progress List-of-Pair generic-arg propagation through `list[i]`.
class JsonObj(val keys: MutableList<String>, val values: MutableList<JsonValue>) : JsonValue()

// ─── Parser ────────────────────────────────────────────────────────

class Parser(val src: String) {
    var pos: Int = 0

    fun skipWs() {
        while (pos < src.length) {
            val c = src[pos]
            if (c == ' ' || c == '\n' || c == '\r' || c == '\t') {
                pos++
            } else {
                return
            }
        }
    }

    fun expect(c: Char) {
        if (pos >= src.length || src[pos] != c) {
            throw IllegalStateException("expected '${c}' at pos ${pos}")
        }
        pos++
    }

    fun peek(): Char {
        if (pos >= src.length) {
            throw IllegalStateException("unexpected end of input at pos ${pos}")
        }
        return src[pos]
    }

    fun parseValue(): JsonValue {
        skipWs()
        val c = peek()
        return when (c) {
            '{' -> parseObject()
            '[' -> parseArray()
            '"' -> JsonStr(parseStringLit())
            't', 'f' -> parseBool()
            'n' -> parseNull()
            else -> parseNumber()
        }
    }

    fun parseObject(): JsonObj {
        expect('{')
        val keys = mutableListOf<String>()
        val values = mutableListOf<JsonValue>()
        skipWs()
        if (peek() == '}') {
            pos++
            return JsonObj(keys, values)
        }
        while (true) {
            skipWs()
            val key = parseStringLit()
            skipWs()
            expect(':')
            val value = parseValue()
            keys.add(key)
            values.add(value)
            skipWs()
            val next = peek()
            if (next == ',') {
                pos++
            } else if (next == '}') {
                pos++
                return JsonObj(keys, values)
            } else {
                throw IllegalStateException("expected ',' or '}' at pos ${pos}")
            }
        }
    }

    fun parseArray(): JsonArr {
        expect('[')
        val items = mutableListOf<JsonValue>()
        skipWs()
        if (peek() == ']') {
            pos++
            return JsonArr(items)
        }
        while (true) {
            items.add(parseValue())
            skipWs()
            val next = peek()
            if (next == ',') {
                pos++
            } else if (next == ']') {
                pos++
                return JsonArr(items)
            } else {
                throw IllegalStateException("expected ',' or ']' at pos ${pos}")
            }
        }
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
            if (c == '\\') {
                pos++
                if (pos >= src.length) {
                    throw IllegalStateException("dangling escape at end of input")
                }
                val e = src[pos]
                pos++
                when (e) {
                    '"' -> sb.append('"')
                    '\\' -> sb.append('\\')
                    'n' -> sb.append('\n')
                    't' -> sb.append('\t')
                    'r' -> sb.append('\r')
                    else -> sb.append(e)
                }
            } else {
                sb.append(c)
                pos++
            }
        }
        throw IllegalStateException("unterminated string literal")
    }

    fun parseBool(): JsonBool {
        val c = peek()
        if (c == 't') {
            expect('t'); expect('r'); expect('u'); expect('e')
            return JsonBool(true)
        }
        expect('f'); expect('a'); expect('l'); expect('s'); expect('e')
        return JsonBool(false)
    }

    fun parseNull(): JsonValue {
        expect('n'); expect('u'); expect('l'); expect('l')
        return JsonNull
    }

    fun parseNumber(): JsonNum {
        var sign = 1
        if (peek() == '-') {
            sign = -1
            pos++
        }
        var n = 0
        var any = false
        while (pos < src.length) {
            val c = src[pos]
            if (c >= '0' && c <= '9') {
                n = n * 10 + (c.code - '0'.code)
                pos++
                any = true
            } else {
                break
            }
        }
        if (!any) {
            throw IllegalStateException("expected digit at pos ${pos}")
        }
        return JsonNum(sign * n)
    }
}

fun parseJson(src: String): JsonValue {
    val p = Parser(src)
    val v = p.parseValue()
    p.skipWs()
    return v
}
