import androidx.compose.runtime.Composable

enum class V { A, B }

fun pick(fn: (V) -> Int): Int = fn(V.A)

@Composable
fun Box() {
    val x = pick { if (it == V.A) 1 else 2 }
}
