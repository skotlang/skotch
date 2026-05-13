import androidx.compose.runtime.Composable

@Composable
inline fun Tagged(id: Int, visible: Boolean, content: () -> Unit) {
    content()
}
