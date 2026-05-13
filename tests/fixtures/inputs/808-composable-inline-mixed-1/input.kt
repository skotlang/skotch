import androidx.compose.runtime.Composable

@Composable
inline fun OnPress(label: String, onClick: () -> Unit) {
    onClick()
}
