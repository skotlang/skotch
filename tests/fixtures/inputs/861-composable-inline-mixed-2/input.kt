import androidx.compose.runtime.Composable

@Composable
inline fun Labeled(label: String, count: Int, content: () -> Unit) {
    content()
}
