import androidx.compose.runtime.Composable

@Composable
inline fun MaybeContent(show: Boolean, content: () -> Unit) {
    if (show) {
        content()
    }
}
