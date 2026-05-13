import androidx.compose.runtime.Composable

@Composable
inline fun Show(visible: Boolean, content: () -> Unit) {
    if (visible) content()
}
