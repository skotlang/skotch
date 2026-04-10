// TODO: object singletons (single INSTANCE field + private constructor).
object Singleton {
    fun greet() {
        println("hi from singleton")
    }
}

fun main() {
    Singleton.greet()
}
