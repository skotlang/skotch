class Ledger {
    private var balance: Int = 0
    val history: MutableList<Int> = mutableListOf()
    fun add(n: Int) {
        balance += n
        history.add(balance)
    }
    fun report(): String = "$balance (${history.size} entries)"
}

fun main() {
    val l = Ledger()
    l.add(10)
    l.add(5)
    l.add(-3)
    l.add(20)
    println(l.report())
    println(l.history)
}
