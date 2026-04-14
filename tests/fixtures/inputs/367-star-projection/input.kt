class Holder<T>(val value: T)

fun printHolder(h: Holder<*>) {
    println(h.value)
}

fun main() {
    printHolder(Holder(42))
    printHolder(Holder("hello"))
}
