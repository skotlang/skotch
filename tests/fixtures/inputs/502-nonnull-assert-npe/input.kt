fun main() {
    val s: String? = null
    try {
        val len = s!!.length
        println(len)
    } catch (e: NullPointerException) {
        println("caught NPE")
    }
}
