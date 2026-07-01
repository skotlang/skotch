abstract class Vehicle(val name: String) {
    abstract fun describe(): String
    fun label(): String = "${name}: ${describe()}"
}

class Car(name: String, val mpg: Int) : Vehicle(name) {
    override fun describe(): String = "car ${mpg}mpg"
}

class Bike(name: String) : Vehicle(name) {
    override fun describe(): String = "bike (no fuel)"
}

fun main() {
    println(Car("tesla", 90).label())
    println(Bike("schwinn").label())
}
