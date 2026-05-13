import androidx.compose.runtime.Composable

@Composable
inline fun M(b: Boolean, s: String, i: Int, content: () -> Unit) {
    content()
}
