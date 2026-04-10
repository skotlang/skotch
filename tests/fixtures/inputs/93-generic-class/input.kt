class Box<T>(val value: T) {
    fun get(): T = value
    override fun toString(): String = "Box($value)"
}

fun main() {
    val intBox = Box(42)
    val strBox = Box("hello")
    println(intBox)
    println(strBox.get())
}
