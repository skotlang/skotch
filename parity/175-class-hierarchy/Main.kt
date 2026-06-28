open class Employee(val name: String, val base: Int) {
    open fun salary(): Int = base
    fun describe(): String = "$name: \$${salary()}"
}

class Manager(name: String, base: Int, val bonus: Int) : Employee(name, base) {
    override fun salary(): Int = base + bonus
}

class Director(name: String, base: Int, val equity: Int) : Employee(name, base) {
    override fun salary(): Int = base * 2 + equity
}

fun main() {
    val emps: List<Employee> = listOf(
        Employee("alice", 5),
        Manager("bob", 7, 3),
        Director("carol", 10, 50)
    )
    for (e in emps) println(e.describe())
}
