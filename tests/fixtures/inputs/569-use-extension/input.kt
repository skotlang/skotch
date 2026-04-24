class DbConn {
    fun close() {
        println("closed")
    }
}

fun process(conn: DbConn): String {
    return "processed"
}

fun main() {
    val conn = DbConn()
    conn.use {
        println("working")
        println(process(conn))
    }
    println("done")
}
