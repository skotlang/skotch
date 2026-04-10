// TODO: default arguments. kotlinc emits a synthetic `$default` overload.
fun greet(name: String = "world") {
    println("Hello, $name!")
}
