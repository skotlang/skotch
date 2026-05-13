import androidx.compose.runtime.Composable

@Composable
inline fun Everything(b: Boolean, i: Int, l: Long, f: Float, d: Double, s: String, content: () -> Unit) {
    content()
}
