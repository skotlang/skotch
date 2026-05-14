import androidx.compose.runtime.Composable

enum class V { A, B }

fun pick(fn: (V) -> Float): Float = fn(V.A)

@Composable
fun Box() {
    val x = pick { if (it == V.A) 1.0f else 2.0f }
}
