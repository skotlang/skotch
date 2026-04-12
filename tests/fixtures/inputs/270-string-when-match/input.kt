fun greetByLang(lang: String): String = when (lang) {
    "en" -> "Hello"
    "es" -> "Hola"
    "fr" -> "Bonjour"
    "de" -> "Hallo"
    else -> "Hi"
}

fun main() {
    println(greetByLang("en"))
    println(greetByLang("es"))
    println(greetByLang("fr"))
    println(greetByLang("de"))
    println(greetByLang("ja"))
}
