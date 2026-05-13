import androidx.compose.runtime.Composable

@Composable
inline fun NumberedContent(n: Int, content: () -> Unit) {
    content()
}
