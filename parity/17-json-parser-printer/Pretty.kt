// Pretty-printer for JsonValue. Recursive walk that accumulates into
// a single StringBuilder; indentation is threaded as a depth parameter.

fun escapeString(s: String): String {
    val sb = StringBuilder()
    sb.append('"')
    var i = 0
    while (i < s.length) {
        val c = s[i]
        when (c) {
            '"' -> sb.append("\\\"")
            '\\' -> sb.append("\\\\")
            '\n' -> sb.append("\\n")
            '\t' -> sb.append("\\t")
            '\r' -> sb.append("\\r")
            else -> sb.append(c)
        }
        i++
    }
    sb.append('"')
    return sb.toString()
}

fun indent(sb: StringBuilder, depth: Int) {
    var i = 0
    while (i < depth) {
        sb.append("  ")
        i++
    }
}

fun prettyInto(v: JsonValue, sb: StringBuilder, depth: Int) {
    when (v) {
        is JsonNull -> sb.append("null")
        is JsonBool -> sb.append(if (v.value) "true" else "false")
        is JsonNum -> sb.append(v.value.toString())
        is JsonStr -> sb.append(escapeString(v.value))
        is JsonArr -> {
            if (v.items.isEmpty()) {
                sb.append("[]")
                return
            }
            sb.append("[\n")
            var i = 0
            while (i < v.items.size) {
                indent(sb, depth + 1)
                prettyInto(v.items[i], sb, depth + 1)
                if (i + 1 < v.items.size) sb.append(",")
                sb.append("\n")
                i++
            }
            indent(sb, depth)
            sb.append("]")
        }
        is JsonObj -> {
            if (v.keys.isEmpty()) {
                sb.append("{}")
                return
            }
            sb.append("{\n")
            var i = 0
            while (i < v.keys.size) {
                indent(sb, depth + 1)
                sb.append(escapeString(v.keys[i]))
                sb.append(": ")
                prettyInto(v.values[i], sb, depth + 1)
                if (i + 1 < v.keys.size) sb.append(",")
                sb.append("\n")
                i++
            }
            indent(sb, depth)
            sb.append("}")
        }
    }
}

fun pretty(v: JsonValue): String {
    val sb = StringBuilder()
    prettyInto(v, sb, 0)
    return sb.toString()
}
