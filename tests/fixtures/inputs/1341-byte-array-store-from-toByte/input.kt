// Lock-in for parity/101-hash work: writing to a `ByteArray` slot
// must always emit `bastore`, regardless of whether the value's MIR
// type tracked its primitive-ness perfectly. Pre-fix the opcode
// selection consulted only the value side; an unresolved RHS that
// fell back to `Ty::Any` produced `aastore`, and the JVM rejected
// the class with `Bad type on operand stack`. Also locks in
// `i.toByte()` → `i2b` opcode (was: unresolved → null placeholder).
fun main() {
    val buf = ByteArray(4)
    buf[0] = 0x80.toByte()
    buf[1] = 0x7f.toByte()
    buf[2] = (-1).toByte()
    buf[3] = 42.toByte()
    // Index each slot explicitly (not via for-in) — the for-in on
    // ByteArray currently erases the element type, which causes a
    // separate "unresolved .toInt() on Any" issue tracked in
    // [[project_for_in_bytearray_element_type]].
    println((buf[0].toInt()) and 0xff)
    println((buf[1].toInt()) and 0xff)
    println((buf[2].toInt()) and 0xff)
    println((buf[3].toInt()) and 0xff)
}
