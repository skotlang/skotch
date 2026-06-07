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
