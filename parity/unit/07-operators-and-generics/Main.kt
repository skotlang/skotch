fun main() {
    val cart = Inventory()
    cart.add("apple", 30.cents())
    cart.add("bread", 4.dollars())
    cart.add("milk", 3.dollars() + 50.cents())

    println("items: ${cart.size()}")
    println("cart: ${cart.describe()}")
    println("total: ${cart.total().format()}")

    val triple = 4.dollars() * 3
    println("4 dollars x 3 = ${triple.format()}")

    val a = 5.dollars()
    val b = 5.dollars()
    println("a == b? ${a == b}")
    val c = 7.dollars()
    println("a == c? ${a == c}")
}
