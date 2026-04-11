fun main() {
    // 2x2 matrix multiply: [[1,2],[3,4]] * [[5,6],[7,8]]
    val a11 = 1; val a12 = 2; val a21 = 3; val a22 = 4
    val b11 = 5; val b12 = 6; val b21 = 7; val b22 = 8

    val c11 = a11 * b11 + a12 * b21
    val c12 = a11 * b12 + a12 * b22
    val c21 = a21 * b11 + a22 * b21
    val c22 = a21 * b12 + a22 * b22

    println(c11)
    println(c12)
    println(c21)
    println(c22)
}
