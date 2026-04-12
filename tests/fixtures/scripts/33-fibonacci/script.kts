var a = 0
var b = 1
for (i in 0..9) {
    println(a)
    val temp = a + b
    a = b
    b = temp
}
