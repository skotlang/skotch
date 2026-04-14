class Producer<out T>(val value: T)

fun main() {
    val p = Producer("hello")
    println(p.value)
    val q = Producer(42)
    println(q.value)
}
