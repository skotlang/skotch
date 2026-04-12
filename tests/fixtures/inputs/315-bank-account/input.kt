class BankAccount(var balance: Int) {
    fun deposit(amount: Int) { balance = balance + amount }
    fun withdraw(amount: Int) { balance = balance - amount }
}

fun main() {
    val a = BankAccount(100)
    a.deposit(50)
    a.withdraw(30)
    println(a.balance)
}
