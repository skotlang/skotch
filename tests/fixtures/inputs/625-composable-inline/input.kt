import androidx.compose.runtime.Composable

@Composable
inline fun Tag(content: () -> Unit) {
    content()
}
