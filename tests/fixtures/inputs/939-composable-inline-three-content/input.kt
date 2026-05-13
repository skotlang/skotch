import androidx.compose.runtime.Composable

@Composable
inline fun Layout(top: () -> Unit, body: () -> Unit, bottom: () -> Unit) {
    top()
    body()
    bottom()
}
