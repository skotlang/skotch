import androidx.compose.runtime.Composable

@Composable
inline fun WithIdAndContent(id: Int, content: () -> Unit) {
    content()
}
