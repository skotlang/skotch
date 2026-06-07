// Brainfuck interpreter — 8-op tape VM.
//
// Commands:
//   >  move data pointer right
//   <  move data pointer left
//   +  increment cell at data pointer
//   -  decrement cell at data pointer
//   .  output the cell as a Char
//   ,  read one byte from input (we model input as a String,
//      with a `readPtr` index)
//   [  jump past matching ']' if cell == 0
//   ]  jump back to matching '[' if cell != 0
//
// Sophistication step over example 25:
//   - char-based dispatch (if/else-if chain) over 8 commands
//     inside a tight interpret loop
//   - balanced-bracket scanning via two helper functions
//     (forward + backward) — exercises stateful while loops
//     in helpers without the var-field-on-class staleness
//   - `(Int).toChar()` conversion for output

fun deltaForwardScan(c: Char): Int {
    if (c == '[') return 1
    if (c == ']') return -1
    return 0
}

fun deltaBackwardScan(c: Char): Int {
    if (c == ']') return 1
    if (c == '[') return -1
    return 0
}

fun findMatchingClose(program: String, openPos: Int): Int {
    var depth = 1
    var pos = openPos
    // Increment FIRST, then scan, so the loop body has no nested
    // `if (depth > 0) pos++` (which skotch's backend mis-emits as a
    // straight-line fall-through to `return` instead of looping back).
    while (depth > 0) {
        pos = pos + 1
        depth = depth + deltaForwardScan(program[pos])
    }
    return pos
}

fun findMatchingOpen(program: String, closePos: Int): Int {
    var depth = 1
    var pos = closePos
    while (depth > 0) {
        pos = pos - 1
        depth = depth + deltaBackwardScan(program[pos])
    }
    return pos
}

// Mutable state passed around as an IntArray so the interpreter
// helpers don't need to be methods on a class with `var` fields
// (which trips skotch's var-field staleness analysis).
// state[0] = ptr, state[1] = pc, state[2] = readPtr.
fun runBrainfuck(program: String, input: String): String {
    val tape = IntArray(30000)
    val state = IntArray(3)
    val out = StringBuilder()
    while (state[1] < program.length) {
        stepBf(program, input, tape, state, out)
    }
    return out.toString()
}

fun stepBf(program: String, input: String, tape: IntArray, state: IntArray, out: StringBuilder) {
    val pc = state[1]
    val ptr = state[0]
    val c = program[pc]
    if (c == ',') {
        readByte(input, tape, ptr, state)
    } else {
        applySimpleOp(c, tape, ptr, out, state)
    }
    state[1] = bracketJump(c, program, tape[state[0]], state[1]) + 1
}

fun applySimpleOp(c: Char, tape: IntArray, ptr: Int, out: StringBuilder, state: IntArray) {
    if (c == '>' || c == '<') {
        movePtr(c, ptr, state)
        return
    }
    if (c == '+' || c == '-') {
        bumpCell(c, tape, ptr)
        return
    }
    if (c == '.') {
        out.append(tape[ptr].toChar())
    }
}

fun movePtr(c: Char, ptr: Int, state: IntArray) {
    if (c == '>') {
        state[0] = ptr + 1
    } else {
        state[0] = ptr - 1
    }
}

fun bumpCell(c: Char, tape: IntArray, ptr: Int) {
    if (c == '+') {
        tape[ptr] = tape[ptr] + 1
    } else {
        tape[ptr] = tape[ptr] - 1
    }
}

fun bracketJump(c: Char, program: String, cell: Int, pc: Int): Int {
    if (c == '[' && cell == 0) return findMatchingClose(program, pc)
    if (c == ']' && cell != 0) return findMatchingOpen(program, pc)
    return pc
}


fun readByte(input: String, tape: IntArray, ptr: Int, state: IntArray) {
    val readPtr = state[2]
    if (readPtr < input.length) {
        tape[ptr] = input[readPtr].code
        state[2] = readPtr + 1
    } else {
        tape[ptr] = 0
    }
}
// Run a handful of Brainfuck programs and print their output.

fun main() {
    // Print 'A' (ASCII 65)
    println(runBrainfuck("+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++.", ""))

    // Hello World — the classic
    val helloWorld = "++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++."
    print(runBrainfuck(helloWorld, ""))
    println()

    // Read first input char, output it twice
    println(runBrainfuck(",..", "Z"))

    // Add two single-digit ASCII digits. Read two bytes, add the
    // second into the first via the standard [<+>-] move loop, then
    // subtract one '0' (48) — the two ASCII bias terms cancel one
    // copy of '0', and the remaining 48 produces the ASCII digit
    // for the sum. Inputs "3" and "4" → "7".
    val addProgram = ",>,[<+>-]<------------------------------------------------."
    println(runBrainfuck(addProgram, "34"))
}
