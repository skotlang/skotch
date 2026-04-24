private fun secret(): String = "hidden"
fun publicGreet(): String = "public: ${secret()}"
