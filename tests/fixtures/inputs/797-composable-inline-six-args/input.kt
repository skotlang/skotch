import androidx.compose.runtime.Composable

@Composable
inline fun Six(a: Int, b: Int, c: Int, d: Int, e: Int, content: () -> Unit) {
    content()
}
