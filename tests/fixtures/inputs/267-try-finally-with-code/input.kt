fun main() {
    println("before")
    try {
        println("in try")
        val x = 42
        println(x)
    } finally {
        println("in finally")
    }
    println("after")
}
