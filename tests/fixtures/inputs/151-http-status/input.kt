fun httpStatus(code: Int): String = when (code) {
    200 -> "OK"
    201 -> "Created"
    400 -> "Bad Request"
    404 -> "Not Found"
    500 -> "Internal Server Error"
    else -> "Unknown"
}

fun main() {
    println(httpStatus(200))
    println(httpStatus(404))
    println(httpStatus(500))
    println(httpStatus(999))
}
