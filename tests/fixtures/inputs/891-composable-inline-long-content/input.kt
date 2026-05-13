import androidx.compose.runtime.Composable

@Composable
inline fun WithId(id: Long, content: () -> Unit) {
    content()
}
