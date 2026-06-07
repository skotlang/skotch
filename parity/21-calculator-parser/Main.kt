// Run the lexer + parser + evaluator on a small set of expressions.
// Output `<source> => <ast> = <value>` for each.

fun evaluate(src: String): String {
    val tokens = tokenize(src)
    val ast = parse(tokens)
    return src + " => " + ast.show() + " = " + ast.eval()
}

fun main() {
    println(evaluate("1 + 2"))
    println(evaluate("3 * 4"))
    println(evaluate("1 + 2 * 3"))
    println(evaluate("(1 + 2) * 3"))
    println(evaluate("10 - 4 - 3"))
    println(evaluate("8 / 2 / 2"))
    println(evaluate("-5 + 7"))
    println(evaluate("2 * -3"))
    println(evaluate("(1 + 2) * (3 + 4)"))
    println(evaluate("100 / (2 + 3) - 5"))
    println(evaluate("-(2 + 3)"))
}
