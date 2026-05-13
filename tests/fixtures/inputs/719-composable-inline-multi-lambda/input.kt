import androidx.compose.runtime.Composable

@Composable
inline fun Group(top: () -> Unit, bottom: () -> Unit, header: () -> Unit) {
    top()
    bottom()
    header()
}
