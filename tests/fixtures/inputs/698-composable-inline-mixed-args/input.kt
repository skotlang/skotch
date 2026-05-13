import androidx.compose.runtime.Composable

@Composable
inline fun Combo(a: Int, b: Boolean, c: String, content: () -> Unit) {
    content()
}
