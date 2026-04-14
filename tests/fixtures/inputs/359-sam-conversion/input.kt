interface Action {
    fun execute(): Unit
}

interface Transform {
    fun apply(x: Int): Int
}

fun runAction(a: Action) {
    a.execute()
}

fun process(t: Transform, v: Int): Int = t.apply(v)

fun main() {
    runAction(Action { println("SAM void!") })
    println(process(Transform { x: Int -> x * 3 }, 7))
}
