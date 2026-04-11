fun toRoman(n: Int): String {
    var result = ""
    var remaining = n

    while (remaining >= 1000) { result = result + "M"; remaining -= 1000 }
    while (remaining >= 900) { result = result + "CM"; remaining -= 900 }
    while (remaining >= 500) { result = result + "D"; remaining -= 500 }
    while (remaining >= 400) { result = result + "CD"; remaining -= 400 }
    while (remaining >= 100) { result = result + "C"; remaining -= 100 }
    while (remaining >= 90) { result = result + "XC"; remaining -= 90 }
    while (remaining >= 50) { result = result + "L"; remaining -= 50 }
    while (remaining >= 40) { result = result + "XL"; remaining -= 40 }
    while (remaining >= 10) { result = result + "X"; remaining -= 10 }
    while (remaining >= 9) { result = result + "IX"; remaining -= 9 }
    while (remaining >= 5) { result = result + "V"; remaining -= 5 }
    while (remaining >= 4) { result = result + "IV"; remaining -= 4 }
    while (remaining >= 1) { result = result + "I"; remaining -= 1 }

    return result
}

fun main() {
    println(toRoman(1))
    println(toRoman(4))
    println(toRoman(9))
    println(toRoman(42))
    println(toRoman(1999))
}
