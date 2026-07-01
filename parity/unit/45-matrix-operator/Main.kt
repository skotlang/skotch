// Drives Matrix + algorithms across the three files. Each block
// exercises a different facet of multi-arg `[]` operator dispatch.

fun main() {
    // Basic ops — single-cell read/write through m[r, c].
    val m = Matrix(3, 3)
    m[1, 1] = 5
    m[0, 2] = 7
    m[2, 0] = 9
    println(m[1, 1])                       // 5
    println(m[0, 2])                       // 7
    println(m[2, 0])                       // 9
    println(m[0, 0])                       // 0 (default)
    println(m.sum())                       // 5+7+9 = 21
    println("---")

    // Identity matrix — cross-file factory using m[i, i] = 1 in a loop.
    val id = identity(4)
    println(id[0, 0])                      // 1
    println(id[1, 1])                      // 1
    println(id[2, 2])                      // 1
    println(id[3, 3])                      // 1
    println(id[0, 1])                      // 0
    println(id[2, 3])                      // 0
    println(trace(id))                     // 4
    println("---")

    // Transpose — cross-file algorithm with nested loop + multi-arg
    // get + multi-arg set on different matrices.
    val src = Matrix(2, 3)
    src[0, 0] = 1
    src[0, 1] = 2
    src[0, 2] = 3
    src[1, 0] = 4
    src[1, 1] = 5
    src[1, 2] = 6
    val t = transpose(src)
    println(t.rows)                        // 3 (swapped)
    println(t.cols)                        // 2 (swapped)
    println(t[0, 0])                       // 1
    println(t[1, 0])                       // 2
    println(t[2, 0])                       // 3
    println(t[0, 1])                       // 4
    println(t[1, 1])                       // 5
    println(t[2, 1])                       // 6
    println("---")

    // Fill — uniform value via the method.
    val f = Matrix(2, 2)
    f.fill(7)
    println(f[0, 0])                       // 7
    println(f[0, 1])                       // 7
    println(f[1, 0])                       // 7
    println(f[1, 1])                       // 7
    println(f.sum())                       // 28
}
