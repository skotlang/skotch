import androidx.compose.runtime.Composable

@Composable
inline fun M(s: String, n: Int, content: () -> Unit) {
    content()
}
