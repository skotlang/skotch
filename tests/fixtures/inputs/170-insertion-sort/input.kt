fun main() {
    var a = 5
    var b = 2
    var c = 8
    var d = 1
    var e = 4
    
    // Simple bubble sort of 5 variables
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
