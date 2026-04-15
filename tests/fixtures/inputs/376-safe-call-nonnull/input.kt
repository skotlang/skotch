fun main() {
    val x: String? = "hello"
    println(x?.length)
    println(x!!.uppercase())

    val y: String? = null
    println(y?.length)
}
