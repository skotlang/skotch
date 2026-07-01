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
