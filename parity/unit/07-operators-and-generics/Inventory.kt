// A simple inventory class — exercises mutableListOf with typed Money
// element access via explicit `as Money` cast (skotch's `List.get(i)`
// returns Object at the descriptor level; the cast restores the
// element class so virtual dispatch works in the body).
class Inventory {
    private val names: MutableList<String> = mutableListOf()
    private val prices: MutableList<Money> = mutableListOf()

    fun add(item: String, price: Money) {
        names.add(item)
        prices.add(price)
    }

    fun size(): Int = names.size

    fun total(): Money {
        var sum = Money(0L)
        var i = 0
        while (i < prices.size) {
            sum = sum + (prices[i] as Money)
            i++
        }
        return sum
    }

    fun describe(): String {
        val parts = mutableListOf<String>()
        var i = 0
        while (i < names.size) {
            val name = names[i] as String
            val price = prices[i] as Money
            parts.add("$name=${price.format()}")
            i++
        }
        return parts.joinToString(", ")
    }
}
