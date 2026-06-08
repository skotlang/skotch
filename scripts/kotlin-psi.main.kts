#!/usr/bin/env kotlin

// Dumps the Kotlin PSI for each file passed on the command line.
//
// Default output: org.jetbrains.kotlin.com.intellij.psi.impl.DebugUtil.psiToString
// With --yaml:    bespoke YAML that completely captures the source — every
//                 leaf node's text is preserved verbatim (including whitespace,
//                 EOL_COMMENT, BLOCK_COMMENT, DOC_COMMENT), so concatenating
//                 every leaf's `text` in pre-order reconstructs the input
//                 byte-for-byte. The emitter asserts this round-trip before
//                 writing, and fails loudly if it does not hold.
//
// Empty composites (e.g. an IMPORT_LIST with no imports) emit `children: []`
// rather than `text: ""`, so the YAML preserves the structural intent of every
// KtNodeType vs. every KtTokens leaf.
//
// Usage: ./kotlin-psi.main.kts [--yaml] file1.kt [file2.kt ...]
//
// The .main.kts extension is required — kotlin-main-kts is what activates
// @file:DependsOn (resolves the jar via Maven / local Maven cache).
//
// CRLF in source is normalized to LF before parsing (IntelliJ PSI keeps only
// LF internally), so a CRLF source file round-trips through PSI as LF.

@file:DependsOn("org.jetbrains.kotlin:kotlin-compiler-embeddable:2.3.21")
@file:Suppress("DEPRECATION", "DEPRECATION_ERROR", "OPT_IN_USAGE_ERROR")

import java.io.File
import org.jetbrains.kotlin.cli.common.messages.MessageRenderer
import org.jetbrains.kotlin.cli.common.messages.PrintingMessageCollector
import org.jetbrains.kotlin.cli.jvm.compiler.EnvironmentConfigFiles
import org.jetbrains.kotlin.cli.jvm.compiler.KotlinCoreEnvironment
import org.jetbrains.kotlin.com.intellij.openapi.util.Disposer
import org.jetbrains.kotlin.com.intellij.psi.PsiElement
import org.jetbrains.kotlin.com.intellij.psi.PsiErrorElement
import org.jetbrains.kotlin.com.intellij.psi.impl.DebugUtil
import org.jetbrains.kotlin.com.intellij.psi.impl.source.tree.CompositeElement
import org.jetbrains.kotlin.config.CommonConfigurationKeys
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.psi.KtPsiFactory

val outputYaml = args.any { it == "--yaml" || it == "-y" }
val filePaths = args.filter { !it.startsWith("-") }

if (filePaths.isEmpty()) {
    System.err.println("usage: kotlin-psi.main.kts [--yaml] FILE [FILE ...]")
    kotlin.system.exitProcess(2)
}

// -- Compiler environment ------------------------------------------------------

val disposable = Disposer.newDisposable()
val config = CompilerConfiguration().apply {
    put(
        CommonConfigurationKeys.MESSAGE_COLLECTOR_KEY,
        PrintingMessageCollector(System.err, MessageRenderer.PLAIN_FULL_PATHS, false),
    )
}
val env = KotlinCoreEnvironment.createForProduction(
    disposable,
    config,
    EnvironmentConfigFiles.JVM_CONFIG_FILES,
)
val psiFactory = KtPsiFactory(env.project, markGenerated = false)

// -- YAML serialization --------------------------------------------------------

// Double-quoted YAML 1.2 string escape that preserves every byte of the source.
// \xHH for C0 control chars and DEL; printable UTF-8 passes through unchanged
// (YAML's double-quoted strings round-trip arbitrary Unicode).
fun yamlString(s: String): String {
    val sb = StringBuilder("\"")
    for (c in s) {
        when {
            c == '"' -> sb.append("\\\"")
            c == '\\' -> sb.append("\\\\")
            c == '\n' -> sb.append("\\n")
            c == '\r' -> sb.append("\\r")
            c == '\t' -> sb.append("\\t")
            c.code < 0x20 || c.code == 0x7F ->
                sb.append("\\x").append(String.format("%02x", c.code))
            else -> sb.append(c)
        }
    }
    sb.append("\"")
    return sb.toString()
}

// A node is composite iff its ASTNode is a CompositeElement (KtNodeTypes,
// IFileElementType, KDoc node types, etc.); otherwise it's a LeafElement
// (KtTokens — keywords, punctuation, IDENTIFIER, INTEGER_LITERAL, REGULAR_STRING_PART,
// WHITE_SPACE, EOL_COMMENT, BLOCK_COMMENT, DOC_COMMENT, KDOC_TEXT, …). The
// firstChild check alone is insufficient because an empty composite has no
// children but still represents a structural node, not a token.
fun isComposite(psi: PsiElement): Boolean = psi.node is CompositeElement

fun typeOf(psi: PsiElement): String = psi.node.elementType.toString()

// Walks PSI via firstChild/nextSibling so whitespace + comments are preserved.
//
// Output shape:
//   - type: "<IElementType.toString()>"          # always present
//     text: "<verbatim leaf text>"               # for leaves only
//     error: "<PsiErrorElement description>"     # for ERROR_ELEMENT only
//     children:                                  # for composites only
//       - type: ...
//     children: []                               # for empty composites
fun emitNode(out: StringBuilder, psi: PsiElement, indent: String, listItem: Boolean) {
    val head = if (listItem) "$indent- " else indent
    val field = if (listItem) "$indent  " else indent

    out.append(head).append("type: ").append(yamlString(typeOf(psi))).append('\n')

    if (psi is PsiErrorElement) {
        out.append(field).append("error: ").append(yamlString(psi.errorDescription)).append('\n')
    }

    if (isComposite(psi)) {
        val first = psi.firstChild
        if (first == null) {
            out.append(field).append("children: []\n")
        } else {
            out.append(field).append("children:\n")
            val childIndent = "$field  "
            var c: PsiElement? = first
            while (c != null) {
                emitNode(out, c, childIndent, listItem = true)
                c = c.nextSibling
            }
        }
    } else {
        out.append(field).append("text: ").append(yamlString(psi.text)).append('\n')
    }
}

fun collectLeafText(sb: StringBuilder, psi: PsiElement) {
    if (isComposite(psi)) {
        var c: PsiElement? = psi.firstChild
        while (c != null) {
            collectLeafText(sb, c)
            c = c.nextSibling
        }
    } else {
        sb.append(psi.text)
    }
}

// -- Main loop -----------------------------------------------------------------

var exitCode = 0
try {
    for ((i, path) in filePaths.withIndex()) {
        val file = File(path)
        if (!file.isFile) {
            System.err.println("skip: not a file: $path")
            exitCode = 1
            continue
        }

        // Normalize CRLF/CR to LF before parsing — IntelliJ PSI strips CR
        // internally, and we want the leaf-text reconstruction to match what
        // we hand to the parser, not the on-disk bytes.
        val raw = file.readText()
        val source = raw.replace("\r\n", "\n").replace('\r', '\n')
        val ktFile = psiFactory.createFile(file.name, source)

        if (outputYaml) {
            val reconstructed = StringBuilder().also { collectLeafText(it, ktFile) }.toString()
            if (reconstructed != source) {
                System.err.println(
                    "ERROR: PSI leaf-text round-trip mismatch for $path " +
                        "(source=${source.length} chars, reconstructed=${reconstructed.length} chars). " +
                        "Refusing to emit YAML for a file the parser cannot losslessly represent."
                )
                exitCode = 1
                continue
            }
            if (i > 0) println("---")
            val sb = StringBuilder()
            sb.append("file: ").append(yamlString(file.path)).append('\n')
            sb.append("source_length: ").append(source.length).append('\n')
            sb.append("crlf_normalized: ").append(if (raw != source) "true" else "false").append('\n')
            sb.append("ast:\n")
            emitNode(sb, ktFile, indent = "  ", listItem = false)
            print(sb)
        } else {
            if (i > 0) println()
            println("=== ${file.path} ===")
            println(DebugUtil.psiToString(ktFile, /*showWhitespaces*/ true, /*showRanges*/ false))
        }
    }
} finally {
    Disposer.dispose(disposable)
}

kotlin.system.exitProcess(exitCode)
