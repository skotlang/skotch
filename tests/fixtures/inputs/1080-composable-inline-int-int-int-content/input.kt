import androidx.compose.runtime.Composable

@Composable
inline fun T(a: Int, b: Int, c: Int, content: () -> Unit) {
    content()
}
