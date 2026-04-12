class Logger(val tag: String) {
    init {
        println("Logger initialized: $tag")
    }

    fun log(msg: String) {
        println("[$tag] $msg")
    }
}

fun main() {
    val logger = Logger("APP")
    logger.log("started")
}
