class Box<T>(val value: T) {
    fun get(): T = value
}

fun main() {
    val intBox = Box(42)
    val strBox = Box("hello")
    println(intBox.get())
    println(strBox.get())
}
