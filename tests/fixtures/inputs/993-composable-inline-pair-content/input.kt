import androidx.compose.runtime.Composable

@Composable
inline fun Pair(content1: () -> Unit, content2: () -> Unit) {
    content1()
    content2()
}
