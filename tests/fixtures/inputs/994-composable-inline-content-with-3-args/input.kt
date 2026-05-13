import androidx.compose.runtime.Composable

@Composable
inline fun WithThree(a: Int, b: String, c: Boolean, content: () -> Unit) {
    content()
}
