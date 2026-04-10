// TODO: lambdas. JVM target lowers via invokedynamic + LambdaMetafactory.
fun main() {
    val plus1 = { x: Int -> x + 1 }
    println(plus1(5))
}
