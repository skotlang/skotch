suspend fun l(): Long = 42L
suspend fun caller(): Long = l()
