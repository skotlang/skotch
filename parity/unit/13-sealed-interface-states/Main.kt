fun main() {
    // Walk through a state machine cycle starting from Idle.
    var current: State = Idle
    var i = 0
    while (i < 7) {
        println("${i}: ${current.describe()} -> ${handle(current)}")
        current = step(current)
        i++
    }
}
