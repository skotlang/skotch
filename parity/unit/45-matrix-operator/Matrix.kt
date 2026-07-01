// 2D matrix using `operator fun get(r, c)` and `operator fun set
// (r, c, v)` for the natural `m[r, c]` / `m[r, c] = v` syntax.
// Storage is a flat IntArray indexed row-major. Probes:
//   - multi-arg `operator fun get` (2 args, returns Int)
//   - multi-arg `operator fun set` (2 args + value, returns Unit)
//   - bounds-check via Kotlin's natural `m[r, c]` syntax
//   - mutable IntArray field + index arithmetic
//
// Pre-fix: skotch's parser only accepted single-arg `[index]` —
// the second comma in `[r, c]` failed to parse. Fix: parser now
// collects comma-separated index args; if more than one, desugars
// `m[a, b]` to `m.get(a, b)` and `m[a, b] = v` to `m.set(a, b, v)`.

class Matrix(val rows: Int, val cols: Int) {
    val data: IntArray = IntArray(rows * cols)

    operator fun get(r: Int, c: Int): Int = data[r * cols + c]

    operator fun set(r: Int, c: Int, value: Int) {
        data[r * cols + c] = value
    }

    fun fill(value: Int) {
        var i = 0
        while (i < data.size) {
            data[i] = value
            i = i + 1
        }
    }

    fun sum(): Int {
        var total = 0
        var i = 0
        while (i < data.size) {
            total = total + data[i]
            i = i + 1
        }
        return total
    }
}
