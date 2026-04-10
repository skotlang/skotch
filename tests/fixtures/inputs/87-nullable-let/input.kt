fun main() {
    val name: String? = "world"
    name?.let {
        println("Hello, $it!")
    }
    val nothing: String? = null
    nothing?.let {
        println("should not print")
    }
}
