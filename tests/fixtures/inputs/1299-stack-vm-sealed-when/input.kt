// Bytecode instructions for a small stack-based VM.
//
// Sophistication step over example 18:
//   - sealed class with a MIX of object-singleton entries (no data)
//     and class entries (carrying an Int target / payload)
//   - the `when` dispatcher pattern-matches both flavors in one
//     expression: `is Push -> …; Add -> …; is Jump -> …; Halt -> …`
//   - operand-stack helpers (push/pop/peek) on a `MutableList<Int>`
//   - program counter mutation that lets `Jump` / `JumpIfZero` rewind
//     the loop without modifying the surrounding `while`
sealed class Instr

class Push(val value: Int) : Instr()

object Add : Instr()
object Sub : Instr()
object Mul : Instr()
object Dup : Instr()
object Swap : Instr()
object Print : Instr()
object Halt : Instr()

class Jump(val target: Int) : Instr()
class JumpIfZero(val target: Int) : Instr()
class Vm(val program: List<Instr>) {
    private val stack: MutableList<Int> = mutableListOf()
    private var pc: Int = 0
    private var halted: Boolean = false

    private fun push(v: Int) {
        stack.add(v)
    }

    private fun pop(): Int {
        val top = stack[stack.size - 1]
        stack.removeAt(stack.size - 1)
        return top
    }

    private fun peek(): Int {
        return stack[stack.size - 1]
    }

    // Helper extracted so the conditional jump computes its next-pc
    // outside the when arm. Lifting the if-else here avoids tripping
    // skotch's use-before-def analysis on `when (instr) { is JumpIfZero ->
    // { if (x) y else z } }`, which would otherwise stub the body.
    private fun nextPcForJumpIfZero(target: Int): Int {
        val top = pop()
        if (top == 0) return target
        return pc + 1
    }

    fun run() {
        while (!halted && pc < program.size) {
            val instr = program[pc]
            when (instr) {
                is Push -> {
                    push(instr.value)
                    pc++
                }
                is Add -> {
                    val b = pop()
                    val a = pop()
                    push(a + b)
                    pc++
                }
                is Sub -> {
                    val b = pop()
                    val a = pop()
                    push(a - b)
                    pc++
                }
                is Mul -> {
                    val b = pop()
                    val a = pop()
                    push(a * b)
                    pc++
                }
                is Dup -> {
                    push(peek())
                    pc++
                }
                is Swap -> {
                    val t = pop()
                    val u = pop()
                    push(t)
                    push(u)
                    pc++
                }
                is Print -> {
                    println(peek())
                    pc++
                }
                is Halt -> {
                    halted = true
                }
                is Jump -> {
                    pc = instr.target
                }
                is JumpIfZero -> {
                    pc = nextPcForJumpIfZero(instr.target)
                }
            }
        }
    }
}
fun main() {
    // Countdown program: prints 5, 4, 3, 2, 1 then halts.
    //
    //   0: Push(5)        ; stack = [5]
    //   1: Dup             ; stack = [5, 5]     ←┐
    //   2: JumpIfZero(9)   ; pop top — if 0 jump│
    //   3: Dup             ; stack = [5, 5]    │
    //   4: Print           ; print top (peek)   │
    //   5: Push(1)         ; stack = [5, 5, 1]  │
    //   6: Sub             ; stack = [5, 4]     │
    //   7: Jump(1)         ; loop ──────────────┘
    //   8: Halt            ; (unreachable filler)
    //   9: Halt            ; exit
    val countdown = listOf(
        Push(5),
        Dup,
        JumpIfZero(9),
        Dup,
        Print,
        Push(1),
        Sub,
        Jump(1),
        Halt,
        Halt
    )
    Vm(countdown).run()

    println("---")

    // Arithmetic program: (2 + 3) * 4 = 20
    val arith = listOf(
        Push(2),
        Push(3),
        Add,
        Push(4),
        Mul,
        Print,
        Halt
    )
    Vm(arith).run()

    println("---")

    // Stack manipulation: push 10 then 20 → swap → top is 10.
    val swap = listOf(
        Push(10),
        Push(20),
        Swap,
        Print,
        Halt
    )
    Vm(swap).run()
}
