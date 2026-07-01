class Util {
    companion object {
        @JvmStatic
        fun double(n: Int): Int = n * 2

        @JvmStatic
        fun greet(name: String): String = "hi $name"
    }
}

fun main() {
    println(Util.double(7))
    println(Util.greet("you"))
}
