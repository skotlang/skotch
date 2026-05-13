fun base(): Double = 1.0
suspend fun caller(): Double = base() * 2.0
