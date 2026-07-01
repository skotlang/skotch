// Smoke test for parity/103-ktlint — drives the picked subset of
// ktlint's reporter API end-to-end: build a ReporterProviderV2,
// pull a ReporterV2 instance, feed it a couple of synthetic
// KtlintCliError records, and verify the rendered output is the
// shape the upstream module produces. Confirms that the project's
// compiled classes loaded under whichever compiler produced the
// classpath we're running against.

import com.pinterest.ktlint.cli.reporter.core.api.KtlintCliError
import com.pinterest.ktlint.cli.reporter.core.api.KtlintCliError.Status.LINT_CAN_NOT_BE_AUTOCORRECTED
import com.pinterest.ktlint.cli.reporter.core.api.KtlintCliError.Status.FORMAT_IS_AUTOCORRECTED
import com.pinterest.ktlint.cli.reporter.plainsummary.PlainSummaryReporterProvider
import com.pinterest.ktlint.cli.reporter.json.JsonReporterProvider
import java.io.ByteArrayOutputStream
import java.io.PrintStream

private fun captureReporter(
    provider: com.pinterest.ktlint.cli.reporter.core.api.ReporterProviderV2<*>,
    opt: Map<String, String> = emptyMap(),
    block: (com.pinterest.ktlint.cli.reporter.core.api.ReporterV2) -> Unit,
): String {
    val baos = ByteArrayOutputStream()
    val ps = PrintStream(baos, true, "UTF-8")
    val reporter = provider.get(ps, opt)
    reporter.beforeAll()
    block(reporter)
    reporter.afterAll()
    ps.flush()
    return baos.toString("UTF-8")
}

fun main() {
    val a = KtlintCliError(
        line = 10, col = 5,
        ruleId = "no-wildcard-imports",
        detail = "Wildcard import",
        status = LINT_CAN_NOT_BE_AUTOCORRECTED,
    )
    val b = KtlintCliError(
        line = 12, col = 1,
        ruleId = "no-wildcard-imports",
        detail = "Wildcard import",
        status = FORMAT_IS_AUTOCORRECTED,
    )
    val c = KtlintCliError(
        line = 1, col = 1,
        ruleId = "max-line-length",
        detail = "Exceeded max line length (120)",
        status = LINT_CAN_NOT_BE_AUTOCORRECTED,
    )

    println("provider-id=${PlainSummaryReporterProvider().id}")
    println("error-fields=${a.line},${a.col},${a.ruleId},${a.status.name}")
    println("status-count=${KtlintCliError.Status.entries.size}")

    val summary = captureReporter(PlainSummaryReporterProvider()) { reporter ->
        reporter.before("Foo.kt")
        reporter.onLintError("Foo.kt", a)
        reporter.onLintError("Foo.kt", b)
        reporter.onLintError("Foo.kt", c)
        reporter.after("Foo.kt")
    }
    println("--- plain-summary ---")
    print(summary)

    val json = captureReporter(JsonReporterProvider()) { reporter ->
        reporter.before("Foo.kt")
        reporter.onLintError("Foo.kt", a)
        reporter.after("Foo.kt")
        reporter.before("Bar.kt")
        reporter.onLintError("Bar.kt", c)
        reporter.after("Bar.kt")
    }
    println("--- json ---")
    print(json)
}
