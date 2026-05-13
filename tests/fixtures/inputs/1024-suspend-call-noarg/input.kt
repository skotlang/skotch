fun helper(): Int = 42
suspend fun caller(): Int = helper() + 1
