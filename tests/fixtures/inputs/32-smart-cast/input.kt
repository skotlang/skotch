// TODO: smart casts after `is` checks.
fun describe(x: Any) {
    if (x is String) {
        println(x.length)
    }
}
