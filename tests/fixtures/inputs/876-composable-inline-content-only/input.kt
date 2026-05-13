import androidx.compose.runtime.Composable

@Composable
inline fun OnlyContent(content: () -> Unit) {
    content()
}
