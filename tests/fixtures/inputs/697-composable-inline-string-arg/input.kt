import androidx.compose.runtime.Composable

@Composable
inline fun WithLabel(label: String, content: () -> Unit) {
    content()
}
