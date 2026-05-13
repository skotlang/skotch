import androidx.compose.runtime.Composable

@Composable
inline fun Multi(content1: () -> Unit, content2: () -> Unit, content3: () -> Unit, content4: () -> Unit) {
    content1()
    content2()
    content3()
    content4()
}
