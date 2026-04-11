fun main() {
    var a = 64
    var b = 25
    var c = 12
    var d = 22
    var e = 11

    for (pass in 1..4) {
        if (a > b) { val t = a; a = b; b = t }
        if (b > c) { val t = b; b = c; c = t }
        if (c > d) { val t = c; c = d; d = t }
        if (d > e) { val t = d; d = e; e = t }
    }

    println(a)
    println(b)
    println(c)
    println(d)
    println(e)
}
