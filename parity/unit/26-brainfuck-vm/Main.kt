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
