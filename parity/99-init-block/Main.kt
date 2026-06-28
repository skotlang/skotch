class Account(val owner: String, initial: Int) {
    var balance: Int = 0
    init {
        require(initial >= 0) { "negative initial: $initial" }
        balance = initial
        println("opened: $owner with $balance")
    }
    fun deposit(n: Int) { balance += n }
}

fun main() {
    val a = Account("alice", 100)
    a.deposit(50)
    println("${a.owner}: ${a.balance}")
}
