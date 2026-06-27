class C(val name: String) {
    fun greet() = "hi $name"
}

fun main() {
    val arr = arrayOf(C("a"), C("b"), C("c"))
    println(arr[1].greet())
    println(arr[2].greet().length)
}
