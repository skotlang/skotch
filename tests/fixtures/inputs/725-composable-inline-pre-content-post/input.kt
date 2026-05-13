import androidx.compose.runtime.Composable

@Composable
inline fun PrePost(pre: () -> Unit, content: () -> Unit, post: () -> Unit) {
    pre()
    content()
    post()
}
