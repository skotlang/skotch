import kotlin.reflect.KProperty

class Cached<T>(private val compute: () -> T) {
    private var value: T? = null
    operator fun getValue(thisRef: Any?, property: KProperty<*>): T {
        if (value == null) value = compute()
        return value!!
    }
}

fun main() {
    val greeting: String by Cached { "Hello, World!" }
    println(greeting)
}
