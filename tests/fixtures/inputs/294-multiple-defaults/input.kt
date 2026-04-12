fun configure(host: String = "localhost", port: Int = 8080) {
    println("$host:$port")
}

fun main() {
    configure()
    configure("example.com")
    configure("example.com", 443)
}
