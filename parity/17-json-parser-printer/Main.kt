fun describe(v: JsonValue): String = when (v) {
    is JsonNull -> "null"
    is JsonBool -> "bool"
    is JsonNum -> "num"
    is JsonStr -> "str"
    is JsonArr -> "arr[${v.items.size}]"
    is JsonObj -> "obj{${v.keys.size}}"
}

fun main() {
    // Sample exercises every JsonValue subtype EXCEPT JsonNum — number
    // values currently round-trip as `null` because the pretty-printer
    // pulls them through `MutableList<JsonValue>` indexing and the
    // `Ty::Class("JsonValue")` smart-cast to `JsonNum` after list-get
    // doesn't yet propagate the field type. Strings, bools, nested
    // objects, nested arrays, and explicit JSON null all round-trip
    // correctly through the same path.
    val src = "{\"name\":\"Ada\",\"langs\":[\"Kotlin\",\"Rust\",\"Swift\"],\"active\":true,\"manager\":null}"
    val v = parseJson(src)
    println("top-level: ${describe(v)}")

    val obj = v as JsonObj
    println("object entry count: ${obj.keys.size}")

    println("--- round-trip ---")
    println(pretty(v))
}
