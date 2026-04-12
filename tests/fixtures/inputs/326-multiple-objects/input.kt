object Logger {
    fun info(msg: String) {
        println("[INFO] $msg")
    }
}

object Config {
    fun appName(): String = "MyApp"
}

fun main() {
    Logger.info("started")
    Logger.info(Config.appName())
}
