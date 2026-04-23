import java.io.File

fun main() {
    val f = File("/tmp")
    println(f.getName())
    println(f.exists())
    println(f.isDirectory())
}
