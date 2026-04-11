fun tax(income: Int): Int = when {
    income <= 10000 -> 0
    income <= 40000 -> (income - 10000) * 10 / 100
    income <= 80000 -> 3000 + (income - 40000) * 20 / 100
    else -> 11000 + (income - 80000) * 30 / 100
}

fun main() {
    println(tax(5000))
    println(tax(25000))
    println(tax(60000))
    println(tax(100000))
}
