import androidx.compose.runtime.Composable

@Composable
inline fun Wrap2(content: () -> Unit) {
    content()
}
