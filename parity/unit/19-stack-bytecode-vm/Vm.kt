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
