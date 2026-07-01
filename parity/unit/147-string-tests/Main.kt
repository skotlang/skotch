fun main() {
    val s = "hello world"
    println(s.startsWith("hello"))
    println(s.startsWith("world"))
    println(s.endsWith("world"))
    println(s.endsWith("hello"))
    println(s.indexOf("o"))
    println(s.lastIndexOf("o"))
    println(s.substring(6))
    println(s.substring(0, 5))
    println(s.padStart(15, '*'))
    println(s.padEnd(15, '!'))
}
