//! JVM-target golden comparison tests.
//!
//! Dynamically discovers fixtures with committed JVM goldens and verifies:
//! 1. skotch .class output is byte-equal to committed golden
//! 2. Normalized text matches committed skotch.norm.txt

use std::path::PathBuf;

use skotch_driver::{emit, EmitOptions, Target};

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Discover fixtures with committed JVM goldens AND supported status.
fn discover_jvm_golden_fixtures() -> Vec<String> {
    let inputs_dir = workspace_root().join("tests/fixtures/inputs");
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm");

    let mut fixtures = Vec::new();
    let Ok(entries) = std::fs::read_dir(&inputs_dir) else {
        return fixtures;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta_path = entry.path().join("meta.toml");
        let golden = jvm_dir.join(&name).join("skotch.class");

        if let Ok(meta) = std::fs::read_to_string(&meta_path) {
            if !meta.contains("\"supported\"") {
                continue;
            }
        } else {
            continue;
        }

        if !golden.exists() {
            continue;
        }

        fixtures.push(name);
    }

    fixtures.sort();
    fixtures
}

#[test]
fn skotch_self_consistent_with_committed_goldens() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(run_skotch_self_consistent_with_committed_goldens)
        .expect("spawning self_consistent thread")
        .join()
        .expect("self_consistent thread");
}

fn run_skotch_self_consistent_with_committed_goldens() {
    let fixtures = discover_jvm_golden_fixtures();
    let mut failures: Vec<String> = Vec::new();

    for name in &fixtures {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let golden = workspace_root()
            .join("tests/fixtures/expected/jvm")
            .join(name)
            .join("skotch.class");

        let tmp =
            std::env::temp_dir().join(format!("skotch-jvm-cmp-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("InputKt.class");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit(&EmitOptions {
                input: input.clone(),
                output: out.clone(),
                target: Target::Jvm,
                norm_out: None,
            })
        }));

        match result {
            Ok(Ok(())) => {
                let new_bytes = std::fs::read(&out).unwrap();
                let golden_bytes = std::fs::read(&golden).unwrap();
                if new_bytes != golden_bytes {
                    failures.push(format!(
                        "{name}: skotch.class drift ({} vs {} bytes)",
                        new_bytes.len(),
                        golden_bytes.len()
                    ));
                }
            }
            Ok(Err(e)) => {
                failures.push(format!("{name}: compile error: {e}"));
            }
            Err(_) => {
                failures.push(format!("{name}: JVM backend panicked"));
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) drifted from committed skotch.class goldens:\n  - {}\n\nRefresh with: cargo xtask gen-fixtures --target jvm",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn skotch_norm_matches_committed_skotch_norm() {
    let fixtures = discover_jvm_golden_fixtures();
    let mut failures: Vec<String> = Vec::new();

    for name in &fixtures {
        let jvm_dir = workspace_root()
            .join("tests/fixtures/expected/jvm")
            .join(name);
        let golden_class = jvm_dir.join("skotch.class");
        let golden_norm = jvm_dir.join("skotch.norm.txt");
        if !golden_norm.exists() {
            continue;
        }
        let bytes = std::fs::read(&golden_class).unwrap();
        let Ok(normalized) = skotch_classfile_norm::normalize_default(&bytes) else {
            failures.push(format!("{name}: normalize failed"));
            continue;
        };
        let golden_text = std::fs::read_to_string(&golden_norm)
            .unwrap()
            .replace('\r', "");
        let norm_text = normalized.as_text().replace('\r', "");
        if norm_text != golden_text {
            failures.push(format!("{name}: normalizer output drifted"));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) have skotch.norm.txt drift:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

/// Single-suspension-point coroutine transform: a single-suspension-
/// point `suspend fun` must be lowered to a state machine with
/// the shape kotlinc emits — specifically a dispatcher prelude
/// that reuses or creates a synthetic `ContinuationImpl`
/// subclass, a `tableswitch` over the continuation's label, and
/// a companion class next to the wrapper class. We assert the
/// observable properties of the fixture rather than a byte-for-
/// byte match so that future tweaks to the dispatcher's stack
/// layout don't cascade into a giant golden update.
#[test]
fn suspend_state_machine_shape_matches_kotlinc() {
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm/391-suspend-state-machine");
    let skotch_norm_path = jvm_dir.join("skotch.norm.txt");
    let kotlinc_norm_path = jvm_dir.join("kotlinc.norm.txt");
    let continuation_class_path = jvm_dir.join("InputKt$run$1.class");

    if !skotch_norm_path.exists() {
        panic!("missing skotch.norm.txt for 391-suspend-state-machine");
    }

    let skotch_text = std::fs::read_to_string(&skotch_norm_path).unwrap();

    // 1) The state machine must use a tableswitch dispatcher.
    assert!(
        skotch_text.contains("tableswitch"),
        "skotch should emit `tableswitch` for the dispatcher"
    );
    // 2) The dispatcher must check `label & MIN_VALUE` via iand.
    assert!(
        skotch_text.contains("iand"),
        "skotch should emit `iand` against MIN_VALUE in the dispatcher"
    );
    // 3) The resume path must guard on COROUTINE_SUSPENDED with if_acmpne.
    assert!(
        skotch_text.contains("if_acmpne"),
        "skotch should emit `if_acmpne` against the SUSPENDED sentinel"
    );
    // 4) The default case must throw IllegalStateException.
    assert!(
        skotch_text.contains("java/lang/IllegalStateException"),
        "skotch should throw IllegalStateException in the default arm"
    );
    // 5) The continuation class must live next to the wrapper class.
    assert!(
        continuation_class_path.exists(),
        "skotch should emit the InputKt$run$1 continuation class file"
    );

    // 6) If the kotlinc reference exists, both compilers must
    //    agree on the pivotal instruction mnemonics (we compare
    //    the dispatcher/tableswitch/suspend-call neighborhood,
    //    not the full body — kotlinc's access flags, attributes,
    //    and stack-map frames carry metadata we don't reproduce).
    if kotlinc_norm_path.exists() {
        let kotlinc_text = std::fs::read_to_string(&kotlinc_norm_path).unwrap();
        for needle in [
            "instanceof Class(InputKt$run$1)",
            "getfield Field(InputKt$run$1.label:I)",
            "ldc int(-2147483648)",
            "iand",
            "tableswitch",
            "invokestatic Method(kotlin/coroutines/intrinsics/IntrinsicsKt.getCOROUTINE_SUSPENDED:()Ljava/lang/Object;)",
            "invokestatic Method(kotlin/ResultKt.throwOnFailure:(Ljava/lang/Object;)V)",
            "invokestatic Method(InputKt.yield_:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;)",
            "if_acmpne",
            "athrow",
        ] {
            assert!(
                skotch_text.contains(needle),
                "skotch.norm.txt missing `{needle}`"
            );
            assert!(
                kotlinc_text.contains(needle),
                "kotlinc.norm.txt missing `{needle}` — reference toolchain drift?"
            );
        }
    }
}

/// Multi-suspension-point coroutine transform: a suspend function with
/// two suspension points and locals live across both of them must
/// be lowered to a 3-arm state machine with per-live-local spill
/// fields (`I$0`, `I$1`, …) on the synthetic continuation class.
/// We check the observable shape: 3 tableswitch cases, spill-field
/// putfields/getfields, integer arithmetic on the return path,
/// final autobox through Integer.valueOf, and structural agreement
/// with the kotlinc reference.
#[test]
fn suspend_multi_point_shape_matches_kotlinc() {
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm/392-suspend-multi-point");
    let skotch_norm_path = jvm_dir.join("skotch.norm.txt");
    let kotlinc_norm_path = jvm_dir.join("kotlinc.norm.txt");
    let continuation_class_path = jvm_dir.join("InputKt$run$1.class");

    if !skotch_norm_path.exists() {
        panic!("missing skotch.norm.txt for 392-suspend-multi-point");
    }

    let skotch_text = std::fs::read_to_string(&skotch_norm_path).unwrap();

    // 1) A 3-arm tableswitch (cases 0, 1, 2).
    assert!(
        skotch_text.contains("tableswitch default=")
            && skotch_text.contains("low=0 high=2")
            && skotch_text.contains("0=")
            && skotch_text.contains("1=")
            && skotch_text.contains("2="),
        "skotch should emit a 3-arm tableswitch for 2 suspend points"
    );
    // 2) Spill fields land as putfield/getfield on I$0 AND I$1.
    for field in ["I$0", "I$1"] {
        assert!(
            skotch_text.contains(&format!("putfield Field(InputKt$run$1.{field}:I)")),
            "skotch should spill into {field}"
        );
        assert!(
            skotch_text.contains(&format!("getfield Field(InputKt$run$1.{field}:I)")),
            "skotch should restore from {field}"
        );
    }
    // 3) Integer addition on the resume tail.
    assert!(
        skotch_text.contains("iadd"),
        "skotch should emit `iadd` for `x + y` on the resume tail"
    );
    // 4) Autobox to Integer before areturn.
    assert!(
        skotch_text.contains(
            "invokestatic Method(java/lang/Integer.valueOf:(I)Ljava/lang/Integer;)"
        ) || skotch_text.contains(
            "invokestatic Method(kotlin/coroutines/jvm/internal/Boxing.boxInt:(I)Ljava/lang/Integer;)"
        ),
        "skotch should autobox the returned int to Integer (Integer.valueOf or Boxing.boxInt)"
    );
    // 5) The continuation class exists and carries the spill fields.
    assert!(
        continuation_class_path.exists(),
        "skotch should emit the InputKt$run$1 continuation class file"
    );
    let cont_bytes = std::fs::read(&continuation_class_path).unwrap();
    let cont_norm = skotch_classfile_norm::normalize_default(&cont_bytes)
        .map_err(|e| format!("normalizing continuation class: {e}"))
        .unwrap();
    let cont_text = cont_norm.as_text();
    assert!(
        cont_text.contains("I$0") && cont_text.contains("I$1"),
        "continuation class should declare I$0 and I$1 spill fields"
    );

    // 6) When kotlinc's reference is available, verify both compilers
    //    agree on the pivotal shape landmarks. We
    //    deliberately allow the Boxing.boxInt / Integer.valueOf
    //    divergence (functionally identical) and don't pin exact
    //    offsets — just that the structural anchors are present on
    //    both sides.
    if kotlinc_norm_path.exists() {
        let kotlinc_text = std::fs::read_to_string(&kotlinc_norm_path).unwrap();
        for needle in [
            "instanceof Class(InputKt$run$1)",
            "getfield Field(InputKt$run$1.label:I)",
            "getfield Field(InputKt$run$1.I$0:I)",
            "getfield Field(InputKt$run$1.I$1:I)",
            "putfield Field(InputKt$run$1.I$0:I)",
            "putfield Field(InputKt$run$1.I$1:I)",
            "tableswitch",
            "invokestatic Method(kotlin/coroutines/intrinsics/IntrinsicsKt.getCOROUTINE_SUSPENDED:()Ljava/lang/Object;)",
            "invokestatic Method(kotlin/ResultKt.throwOnFailure:(Ljava/lang/Object;)V)",
            "invokestatic Method(InputKt.yield_:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;)",
            "if_acmpne",
            "iadd",
            "athrow",
        ] {
            assert!(
                skotch_text.contains(needle),
                "skotch.norm.txt missing `{needle}`"
            );
            assert!(
                kotlinc_text.contains(needle),
                "kotlinc.norm.txt missing `{needle}` — reference toolchain drift?"
            );
        }
    }
}

/// CPS signature transform: every `suspend fun` must
/// acquire a trailing `$completion: Continuation` parameter and
/// return `java.lang.Object`, matching kotlinc's CPS signature
/// half. We verify the descriptor byte-for-byte against the
/// committed `kotlinc.norm.txt` so any future regression in the
/// signature rewrite (missing parameter, wrong return type, etc.)
/// fails loudly. We deliberately compare only the `compute`
/// method line — the method body will diverge from kotlinc (we
/// don't yet emit the full state machine) and access flags also
/// differ (`public static` vs kotlinc's `public static final`).
#[test]
fn suspend_fun_descriptor_matches_kotlinc() {
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm/390-suspend-signature");
    let skotch_norm_path = jvm_dir.join("skotch.norm.txt");
    let kotlinc_norm_path = jvm_dir.join("kotlinc.norm.txt");

    if !skotch_norm_path.exists() {
        panic!("missing skotch.norm.txt for 390-suspend-signature");
    }
    if !kotlinc_norm_path.exists() {
        eprintln!("[skip] kotlinc.norm.txt missing — regenerate with cargo xtask gen-fixtures --fixture 390-suspend-signature");
        return;
    }

    let skotch_text = std::fs::read_to_string(&skotch_norm_path).unwrap();
    let kotlinc_text = std::fs::read_to_string(&kotlinc_norm_path).unwrap();

    let expected_descriptor = "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;";

    let find_compute_descriptor = |text: &str| -> Option<String> {
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("method") && trimmed.contains(" compute ") {
                // Format: "method        compute (Lkotlin/...) 0xNNNN"
                let rest = trimmed.split_once(" compute ").unwrap().1;
                let desc = rest.rsplit_once(' ').map(|(d, _)| d).unwrap_or(rest);
                return Some(desc.trim().to_string());
            }
        }
        None
    };

    let skotch_desc =
        find_compute_descriptor(&skotch_text).expect("skotch.norm.txt: no `compute` method line");
    let kotlinc_desc =
        find_compute_descriptor(&kotlinc_text).expect("kotlinc.norm.txt: no `compute` method line");

    assert_eq!(
        skotch_desc, expected_descriptor,
        "skotch did not emit the post-CPS-signature descriptor"
    );
    assert_eq!(
        kotlinc_desc, expected_descriptor,
        "kotlinc reference descriptor drifted — did the reference toolchain change?"
    );
    assert_eq!(
        skotch_desc, kotlinc_desc,
        "skotch and kotlinc disagree on the `compute` descriptor"
    );
}

/// Suspend lambda transform: a lambda whose body
/// calls a suspend function must be lowered to a class that extends
/// `kotlin/coroutines/jvm/internal/SuspendLambda`, implements
/// `Function1` (arity bumped by one to account for the trailing
/// `Continuation`), and carries the canonical 5-method shell that
/// kotlinc produces:
///   1. `<init>(Continuation)V`
///   2. `invokeSuspend(Object)Object`  (real state machine — no
///      longer a stub; see `emit_suspend_lambda_invoke_suspend_body`
///      in the JVM backend)
///   3. `create(Continuation)Continuation`
///   4. `invoke(Continuation)Object`   (typed Function1 entry)
///   5. `invoke(Object)Object`         (erased bridge)
///
/// We verify the shape structurally (normalizer text assertions) plus
/// the key state-machine moves inside `invokeSuspend`. We don't yet
/// byte-match against kotlinc because we still skip the `Kotlin`
/// metadata, `InnerClasses`, `DebugMetadata`, and Signature attributes
/// kotlinc emits; byte parity with OUR OWN output is tracked via
/// `skotch.class` committed goldens for fixtures marked `supported`.
/// This fixture is still `stub` because the wrapper class instantiates
/// the lambda with a `null` completion, so actually invoking the
/// resulting block would NPE inside SuspendLambda's superclass
/// constructor.
#[test]
fn suspend_lambda_shell_shape() {
    let input = workspace_root().join("tests/fixtures/inputs/394-suspend-lambda-shell/input.kt");
    assert!(input.exists(), "missing fixture input.kt");

    let tmp = std::env::temp_dir().join(format!(
        "skotch-suspend-lambda-shell-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("InputKt.class");

    emit(&EmitOptions {
        input,
        output: out.clone(),
        target: Target::Jvm,
        norm_out: None,
    })
    .expect("compilation should succeed");

    // The lambda class lives alongside the wrapper class.
    let lambda_path = tmp.join("InputKt$Lambda$0.class");
    assert!(
        lambda_path.exists(),
        "skotch should emit the InputKt$Lambda$0 class file"
    );

    // Normalize & inspect the lambda class.
    let lambda_bytes = std::fs::read(&lambda_path).unwrap();
    let norm = skotch_classfile_norm::normalize_default(&lambda_bytes)
        .expect("normalizing lambda class should succeed");
    let text = norm.as_text();

    // 1) Superclass is SuspendLambda.
    assert!(
        text.contains("kotlin/coroutines/jvm/internal/SuspendLambda"),
        "suspend lambda should extend SuspendLambda, got:\n{text}"
    );
    // 2) Implements Function1 (arity bumped by +1 for the completion).
    assert!(
        text.contains("kotlin/jvm/functions/Function1"),
        "suspend lambda should implement Function1 (arity+1), got:\n{text}"
    );
    // 3) All 5 canonical methods are present.
    for (needle, description) in [
        (" <init> (Lkotlin/coroutines/Continuation;)V", "<init>"),
        (
            " invokeSuspend (Ljava/lang/Object;)Ljava/lang/Object;",
            "invokeSuspend",
        ),
        (
            " create (Lkotlin/coroutines/Continuation;)Lkotlin/coroutines/Continuation;",
            "create",
        ),
        (
            " invoke (Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
            "typed invoke(Continuation)",
        ),
        (
            " invoke (Ljava/lang/Object;)Ljava/lang/Object;",
            "bridge invoke(Object)",
        ),
    ] {
        assert!(
            text.contains(needle),
            "suspend lambda shell missing {description} — looked for `{needle}` in:\n{text}"
        );
    }
    // 4) The super-ctor call is SuspendLambda.<init>(I,Continuation)V.
    assert!(
        text.contains(
            "invokespecial Method(kotlin/coroutines/jvm/internal/SuspendLambda.<init>:(ILkotlin/coroutines/Continuation;)V)"
        ),
        "suspend lambda <init> should invokespecial SuspendLambda.<init>(I,Continuation)V"
    );
    // 5) The invokeSuspend body is now a real CPS state machine
    //    (suspend lambda codegen). Verify the key moves by their string
    //    shape in the normalizer output.
    let expected_sm_fragments = [
        // Setup: fetch $SUSPENDED, stash in slot 2, read label off this.
        "invokestatic Method(kotlin/coroutines/intrinsics/IntrinsicsKt.getCOROUTINE_SUSPENDED",
        "getfield Field(InputKt$Lambda$0.label:I)",
        "tableswitch",
        // Case 0: throwOnFailure, checkcast this to Continuation, flip label, invoke callee.
        "invokestatic Method(kotlin/ResultKt.throwOnFailure:(Ljava/lang/Object;)V)",
        "putfield Field(InputKt$Lambda$0.label:I)",
        "invokestatic Method(InputKt.yield_:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;)",
        // SUSPENDED bailout: dup; aload SUSPENDED; if_acmpne … areturn.
        "if_acmpne",
        // Resume tail: pop; load literal; areturn.
        "ldc \"hello\"",
        // Default branch still throws IllegalStateException — the
        // same placeholder the named-suspend-fun dispatcher uses.
        "new Class(java/lang/IllegalStateException)",
        "ldc \"call to 'resume' before 'invoke' with coroutine\"",
    ];
    for needle in expected_sm_fragments {
        assert!(
            text.contains(needle),
            "invokeSuspend state machine missing fragment `{needle}` in:\n{text}"
        );
    }
    // 6) The bridge invoke casts its Object arg to Continuation
    //    before tail-calling the typed invoke.
    assert!(
        text.contains(
            "invokevirtual Method(InputKt$Lambda$0.invoke:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;)"
        ),
        "bridge invoke should delegate to typed invoke(Continuation)"
    );

    // 7) The classfile must pass JVM verification when loaded. We
    //    can't easily run `java -Xverify:all` from the unit test
    //    without additional scaffolding, but we can at least round-
    //    trip through the normalizer as a proxy for structural
    //    validity (it rejects malformed constant pools, truncated
    //    methods, etc.).
    let rt = skotch_classfile_norm::normalize_default(&lambda_bytes);
    assert!(
        rt.is_ok(),
        "lambda class must round-trip through normalizer"
    );

    // Non-suspend lambdas MUST remain byte-stable: ensure the
    // wrapper class still has a makeLambda that news up the lambda
    // with a single Continuation arg (and nothing else).
    let wrapper_bytes = std::fs::read(&out).unwrap();
    let wrapper_norm = skotch_classfile_norm::normalize_default(&wrapper_bytes)
        .expect("wrapper class should normalize");
    let wrapper_text = wrapper_norm.as_text();
    assert!(
        wrapper_text.contains(
            "invokespecial Method(InputKt$Lambda$0.<init>:(Lkotlin/coroutines/Continuation;)V)"
        ),
        "wrapper should instantiate the lambda via <init>(Continuation)V"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Multi-suspension suspend lambda: a suspend lambda with TWO
/// suspension points and primitive-typed local variables (`x`, `y`)
/// that cross the suspend boundaries. kotlinc emits spill fields
/// (`I$0`, `I$1`) directly on the lambda class (the lambda IS the
/// continuation) and a 3-arm `tableswitch` inside `invokeSuspend`.
///
/// We verify the shape structurally. Byte parity with kotlinc is NOT
/// asserted (we still skip the `Kotlin`, `InnerClasses`, `Signature`,
/// and `DebugMetadata` attributes kotlinc emits); byte parity with
/// OUR OWN committed output is tracked via the `supported`-status
/// fixtures' `skotch.class` goldens.
///
/// The fixture is `stub` because the wrapper class still instantiates
/// the lambda with a `null` completion continuation. Running
/// `invoke(…)` on the resulting block would NPE inside the
/// SuspendLambda super-ctor, so we only exercise class loading +
/// structural shape here.
#[test]
fn suspend_lambda_multi_suspend_shape() {
    let input = workspace_root().join("tests/fixtures/inputs/395-suspend-lambda-multi/input.kt");
    assert!(input.exists(), "missing fixture input.kt");

    let tmp = std::env::temp_dir().join(format!(
        "skotch-suspend-lambda-multi-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("InputKt.class");

    emit(&EmitOptions {
        input,
        output: out.clone(),
        target: Target::Jvm,
        norm_out: None,
    })
    .expect("compilation should succeed");

    let lambda_path = tmp.join("InputKt$Lambda$0.class");
    assert!(
        lambda_path.exists(),
        "skotch should emit the InputKt$Lambda$0 class file"
    );

    let lambda_bytes = std::fs::read(&lambda_path).unwrap();
    let norm = skotch_classfile_norm::normalize_default(&lambda_bytes)
        .expect("normalizing lambda class should succeed");
    let text = norm.as_text();

    // 1) Superclass is SuspendLambda, implements Function1.
    assert!(
        text.contains("kotlin/coroutines/jvm/internal/SuspendLambda"),
        "suspend lambda should extend SuspendLambda, got:\n{text}"
    );
    assert!(
        text.contains("kotlin/jvm/functions/Function1"),
        "suspend lambda should implement Function1 (arity+1), got:\n{text}"
    );

    // 2) Spill fields I$0 and I$1 live ON THE LAMBDA (no separate
    //    continuation class).
    for field in ["I$0", "I$1"] {
        assert!(
            text.contains(&format!("Field(InputKt$Lambda$0.{field}:I)")),
            "suspend lambda should declare the spill field `{field}:I` on itself, \
             got:\n{text}"
        );
    }

    // 3) All 5 canonical methods are present.
    for (needle, description) in [
        (" <init> (Lkotlin/coroutines/Continuation;)V", "<init>"),
        (
            " invokeSuspend (Ljava/lang/Object;)Ljava/lang/Object;",
            "invokeSuspend",
        ),
        (
            " create (Lkotlin/coroutines/Continuation;)Lkotlin/coroutines/Continuation;",
            "create",
        ),
        (
            " invoke (Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
            "typed invoke(Continuation)",
        ),
        (
            " invoke (Ljava/lang/Object;)Ljava/lang/Object;",
            "bridge invoke(Object)",
        ),
    ] {
        assert!(
            text.contains(needle),
            "suspend lambda shell missing {description} — looked for `{needle}` in:\n{text}"
        );
    }

    // 4) invokeSuspend carries a 3-arm tableswitch (N sites → N+1
    //    cases). The normalizer prints the switch alongside its
    //    targets, so `tableswitch` appears once per invokeSuspend.
    assert!(
        text.contains("tableswitch"),
        "invokeSuspend should dispatch on label via tableswitch in:\n{text}"
    );

    // 5) Canonical state-machine fragments. Each one pins down a
    //    specific structural anchor of the multi-suspension shape.
    let expected_fragments = [
        // Setup: read $SUSPENDED, stash in slot 2, read label off this.
        "invokestatic Method(kotlin/coroutines/intrinsics/IntrinsicsKt.getCOROUTINE_SUSPENDED",
        "getfield Field(InputKt$Lambda$0.label:I)",
        // Case 0: throwOnFailure; segment (push 10); callee cont arg
        // (`aload_0; checkcast Continuation`); spill I$0; set label=1;
        // invoke yield_.
        "invokestatic Method(kotlin/ResultKt.throwOnFailure:(Ljava/lang/Object;)V)",
        "putfield Field(InputKt$Lambda$0.I$0:I)",
        "putfield Field(InputKt$Lambda$0.label:I)",
        "invokestatic Method(InputKt.yield_:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;)",
        // SUSPENDED check (present on every non-final case).
        "if_acmpne",
        // Case N-1 (second yield): spill I$1 as well.
        "putfield Field(InputKt$Lambda$0.I$1:I)",
        // Final case: `x + y` autoboxed through Integer.valueOf
        // before `areturn`.
        "invokestatic Method(java/lang/Integer.valueOf:(I)Ljava/lang/Integer;)",
        // Default branch: the same IllegalStateException placeholder
        // the named-function dispatcher uses.
        "new Class(java/lang/IllegalStateException)",
        "ldc \"call to 'resume' before 'invoke' with coroutine\"",
    ];
    for needle in expected_fragments {
        assert!(
            text.contains(needle),
            "multi-suspension invokeSuspend missing fragment `{needle}` in:\n{text}"
        );
    }

    // 6) The classfile round-trips through our normalizer (a proxy
    //    for structural validity — rejects malformed constant pools,
    //    truncated methods, etc.).
    assert!(
        skotch_classfile_norm::normalize_default(&lambda_bytes).is_ok(),
        "lambda class must round-trip through normalizer"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Runtime-wired suspend lambdas: suspend lambdas wired at
/// runtime. Verifies the structural shape of:
///
/// 1. `runIt(block: suspend () -> String)` — a suspend function that
///    takes a suspend-typed parameter. Its descriptor must use
///    `Function1` (arity bumped +1 for Continuation), and the body
///    must invoke the block via `invokeinterface Function1.invoke`
///    passing the enclosing `$completion` continuation. No state
///    machine is generated because the only "suspend" dispatch is
///    through the FunctionN interface, not a static suspend call.
///
/// 2. `run_()` — a suspend function that creates a SuspendLambda,
///    passes it to `runIt`, and returns the result. This DOES have a
///    state machine (one suspend site: the call to `runIt`), and the
///    segment before the call must handle `NewInstance` + `Constructor`
///    for the lambda instantiation.
///
/// 3. The lambda class (`InputKt$Lambda$0`) is a SuspendLambda with
///    the canonical 5-method shell.
#[test]
fn suspend_runtime_wiring_shape() {
    let input = workspace_root().join("tests/fixtures/inputs/396-suspend-runtime/input.kt");
    assert!(input.exists(), "missing fixture input.kt");

    let tmp = std::env::temp_dir().join(format!("skotch-suspend-runtime-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("InputKt.class");

    emit(&EmitOptions {
        input,
        output: out.clone(),
        target: Target::Jvm,
        norm_out: None,
    })
    .expect("compilation should succeed");

    // Normalize the wrapper class.
    let wrapper_bytes = std::fs::read(&out).unwrap();
    let wrapper_norm = skotch_classfile_norm::normalize_default(&wrapper_bytes)
        .expect("wrapper class should normalize");
    let text = wrapper_norm.as_text();

    // ── runIt method ──────────────────────────────────────────────
    // 1) Descriptor uses Function1 (arity+1 for Continuation).
    assert!(
        text.contains("runIt (Lkotlin/jvm/functions/Function1;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;"),
        "runIt should accept Function1 + Continuation, got:\n{text}"
    );
    // 2) Body invokes block via invokeinterface Function1.invoke.
    assert!(
        text.contains("invokeinterface InterfaceMethod(kotlin/jvm/functions/Function1.invoke:(Ljava/lang/Object;)Ljava/lang/Object;)"),
        "runIt should dispatch block() via invokeinterface Function1.invoke, got:\n{text}"
    );
    // 3) runIt does NOT have a tableswitch (no state machine).
    // Count tableswitch occurrences — only run_() should have one.
    let run_it_section_end = text.find("method        run_").unwrap_or(text.len());
    let run_it_section = &text[..run_it_section_end];
    assert!(
        !run_it_section.contains("tableswitch"),
        "runIt should NOT have a state machine (tail-call), got:\n{run_it_section}"
    );

    // ── run_ method ───────────────────────────────────────────────
    // 4) run_ has a state machine with tableswitch.
    assert!(
        text.contains("tableswitch"),
        "run_ should have a state machine with tableswitch, got:\n{text}"
    );
    // 5) The continuation class exists.
    let cont_path = tmp.join("InputKt$run_$1.class");
    assert!(
        cont_path.exists(),
        "skotch should emit the InputKt$run_$1 continuation class file"
    );
    // 6) run_ calls runIt with the correct descriptor.
    assert!(
        text.contains("invokestatic Method(InputKt.runIt:(Lkotlin/jvm/functions/Function1;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;)"),
        "run_ should call runIt with Function1 param, got:\n{text}"
    );
    // 7) Lambda is instantiated with Constructor(Continuation)V.
    assert!(
        text.contains(
            "invokespecial Method(InputKt$Lambda$0.<init>:(Lkotlin/coroutines/Continuation;)V)"
        ),
        "run_ should instantiate lambda with <init>(Continuation)V, got:\n{text}"
    );
    // 8) Result is checkcast to String (the declared return type).
    assert!(
        text.contains("checkcast Class(java/lang/String)"),
        "run_ should checkcast the result to String, got:\n{text}"
    );

    // ── Lambda class ──────────────────────────────────────────────
    let lambda_path = tmp.join("InputKt$Lambda$0.class");
    assert!(
        lambda_path.exists(),
        "skotch should emit the InputKt$Lambda$0 class file"
    );
    let lambda_bytes = std::fs::read(&lambda_path).unwrap();
    let lambda_norm = skotch_classfile_norm::normalize_default(&lambda_bytes)
        .expect("lambda class should normalize");
    let lambda_text = lambda_norm.as_text();
    // 9) Lambda extends SuspendLambda.
    assert!(
        lambda_text.contains("kotlin/coroutines/jvm/internal/SuspendLambda"),
        "suspend lambda should extend SuspendLambda, got:\n{lambda_text}"
    );
    // 10) Lambda implements Function1.
    assert!(
        lambda_text.contains("kotlin/jvm/functions/Function1"),
        "suspend lambda should implement Function1, got:\n{lambda_text}"
    );

    // ── All classfiles round-trip through the normalizer ──────────
    for path in [&out, &lambda_path, &cont_path] {
        let bytes = std::fs::read(path).unwrap();
        assert!(
            skotch_classfile_norm::normalize_default(&bytes).is_ok(),
            "{} must round-trip through normalizer",
            path.display()
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── kotlinc parity tracking ────────────────────────────────────────────
//
// This test compares skotch's normalized output against kotlinc's
// normalized output for ALL fixtures that have both goldens. It does
// NOT fail on mismatches — instead it prints a parity report showing
// how many fixtures match exactly vs. diverge, and what the most
// common differences are. This serves as a progress tracker toward
// the goal of exact bytecode parity with kotlinc.

#[test]
fn kotlinc_parity_report() {
    let inputs_dir = workspace_root().join("tests/fixtures/inputs");
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm");

    let mut total = 0u32;
    let mut matching = 0u32;
    let mut method_count_diff = 0u32;
    let mut version_diff = 0u32;
    let mut access_diff = 0u32;
    let mut code_diff = 0u32;

    let Ok(entries) = std::fs::read_dir(&inputs_dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();

    for name in &names {
        let kotlinc_norm = jvm_dir.join(name).join("kotlinc.norm.txt");
        let skotch_norm = jvm_dir.join(name).join("skotch.norm.txt");
        if !kotlinc_norm.exists() || !skotch_norm.exists() {
            continue;
        }
        total += 1;

        let kotlinc_text = std::fs::read_to_string(&kotlinc_norm)
            .unwrap()
            .replace('\r', "");
        let skotch_text = std::fs::read_to_string(&skotch_norm)
            .unwrap()
            .replace('\r', "");

        if kotlinc_text == skotch_text {
            matching += 1;
            continue;
        }

        // Categorize the differences.
        let k_lines: Vec<&str> = kotlinc_text.lines().collect();
        let s_lines: Vec<&str> = skotch_text.lines().collect();

        let k_methods = k_lines.iter().filter(|l| l.starts_with("method")).count();
        let s_methods = s_lines.iter().filter(|l| l.starts_with("method")).count();
        if k_methods != s_methods {
            method_count_diff += 1;
        }

        let k_ver = k_lines.iter().find(|l| l.starts_with("class_version"));
        let s_ver = s_lines.iter().find(|l| l.starts_with("class_version"));
        if k_ver != s_ver {
            version_diff += 1;
        }

        let k_access: Vec<&&str> = k_lines
            .iter()
            .filter(|l| l.contains("access_flags") || (l.starts_with("method") && l.contains("0x")))
            .collect();
        let s_access: Vec<&&str> = s_lines
            .iter()
            .filter(|l| l.contains("access_flags") || (l.starts_with("method") && l.contains("0x")))
            .collect();
        if k_access != s_access {
            access_diff += 1;
        }

        let k_code: Vec<&&str> = k_lines.iter().filter(|l| l.starts_with("    ")).collect();
        let s_code: Vec<&&str> = s_lines.iter().filter(|l| l.starts_with("    ")).collect();
        if k_code != s_code {
            code_diff += 1;
        }
    }

    eprintln!("\n=== kotlinc parity report ===");
    eprintln!("  Total fixtures with both goldens: {total}");
    eprintln!(
        "  Exact matches:                    {matching} ({:.0}%)",
        if total > 0 {
            matching as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    );
    eprintln!("  Divergent:                        {}", total - matching);
    eprintln!("    - class version differs:        {version_diff}");
    eprintln!("    - method count differs:         {method_count_diff}");
    eprintln!("    - access flags differ:          {access_diff}");
    eprintln!("    - bytecode differs:             {code_diff}");
    eprintln!("=============================\n");

    // This test always passes — it's informational.
    // As parity improves, we can tighten to assert matching == total.
}

/// Regression floor for skotch-vs-kotlinc bytecode parity.
///
/// `tests/fixtures/kotlinc_parity_baseline.txt` is a sorted, one-fixture-
/// per-line list of every fixture whose `skotch.norm.txt` was byte-
/// identical to its `kotlinc.norm.txt` at the time the baseline was
/// committed. This test asserts that every name in the file still
/// matches. The only legitimate way to remove a fixture from the
/// baseline is to delete its line in the same PR that introduces the
/// divergence — making such a regression visible in code review
/// instead of hiding behind an informational counter.
///
/// New fixtures are *not* expected to be in the baseline; this test
/// never *fails* on a missing entry, only on a baselined entry that
/// no longer matches. To grow the baseline, run
/// `cargo xtask refresh-parity-baseline` (or regenerate
/// `kotlinc_parity_baseline.txt` from the parity report's "matching"
/// set and commit the diff).
#[test]
fn kotlinc_parity_no_regressions() {
    let baseline_path = workspace_root().join("tests/fixtures/kotlinc_parity_baseline.txt");
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm");

    let baseline_text = match std::fs::read_to_string(&baseline_path) {
        Ok(t) => t,
        Err(_) => {
            // Baseline file is committed; missing it is a configuration
            // bug, not a test failure. Surface clearly.
            panic!(
                "missing baseline at {}\n\
                 — commit the file (run the kotlinc_parity_report and \
                 capture the 'matching' set)",
                baseline_path.display()
            );
        }
    };

    let baseline: Vec<&str> = baseline_text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect();
    let baseline_count = baseline.len();

    let mut regressions: Vec<String> = Vec::new();
    let mut missing_kotlinc: Vec<String> = Vec::new();
    let mut missing_skotch: Vec<String> = Vec::new();

    for name in &baseline {
        let kotlinc_norm = jvm_dir.join(name).join("kotlinc.norm.txt");
        let skotch_norm = jvm_dir.join(name).join("skotch.norm.txt");

        if !kotlinc_norm.exists() {
            missing_kotlinc.push((*name).to_string());
            continue;
        }
        if !skotch_norm.exists() {
            missing_skotch.push((*name).to_string());
            continue;
        }

        let k = std::fs::read_to_string(&kotlinc_norm)
            .unwrap()
            .replace('\r', "");
        let s = std::fs::read_to_string(&skotch_norm)
            .unwrap()
            .replace('\r', "");

        if k != s {
            regressions.push((*name).to_string());
        }
    }

    let total_problems = regressions.len() + missing_kotlinc.len() + missing_skotch.len();
    if total_problems == 0 {
        return;
    }

    let mut msg = format!(
        "kotlinc-parity regression: {} of {} baselined fixtures no longer match\n\n",
        total_problems, baseline_count
    );
    if !regressions.is_empty() {
        msg.push_str(&format!(
            "  Divergent ({}):\n    {}\n",
            regressions.len(),
            regressions.join("\n    ")
        ));
    }
    if !missing_kotlinc.is_empty() {
        msg.push_str(&format!(
            "  Missing kotlinc.norm.txt ({}):\n    {}\n",
            missing_kotlinc.len(),
            missing_kotlinc.join("\n    ")
        ));
    }
    if !missing_skotch.is_empty() {
        msg.push_str(&format!(
            "  Missing skotch.norm.txt ({}):\n    {}\n",
            missing_skotch.len(),
            missing_skotch.join("\n    ")
        ));
    }
    msg.push_str(
        "\nIf the divergence is intentional, remove the affected lines \
         from tests/fixtures/kotlinc_parity_baseline.txt in the same PR \
         that introduces the change and explain why in the commit \
         message.\n",
    );

    panic!("{msg}");
}

/// Aspirational byte-identical fixture set.
///
/// `tests/fixtures/kotlinc_parity_aspirational.txt` lists a small,
/// curated set of fixtures that exercise high-leverage Kotlin idioms
/// where Skotch *must* produce normalized bytecode identical to
/// kotlinc's. Each line is `<fixture-name> # <reason>`. The set is
/// intentionally small so each entry is a deliberate, defended
/// guarantee — not a side-effect of a generated list.
///
/// Failure here means a *high-leverage* idiom regressed, which is a
/// stronger signal than the broad regression-floor test and warrants
/// extra scrutiny in code review.
#[test]
fn kotlinc_parity_aspirational_must_match() {
    let aspirational_path = workspace_root().join("tests/fixtures/kotlinc_parity_aspirational.txt");
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm");

    let text = match std::fs::read_to_string(&aspirational_path) {
        Ok(t) => t,
        Err(_) => {
            panic!(
                "missing aspirational set at {}",
                aspirational_path.display()
            );
        }
    };

    let mut entries: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, reason) = match line.split_once('#') {
            Some((n, r)) => (n.trim().to_string(), r.trim().to_string()),
            None => (line.to_string(), String::new()),
        };
        entries.push((name, reason));
    }

    let mut failures: Vec<String> = Vec::new();
    for (name, reason) in &entries {
        let kotlinc_norm = jvm_dir.join(name).join("kotlinc.norm.txt");
        let skotch_norm = jvm_dir.join(name).join("skotch.norm.txt");

        if !kotlinc_norm.exists() || !skotch_norm.exists() {
            failures.push(format!(
                "{name}: missing kotlinc.norm.txt or skotch.norm.txt (reason: {reason})"
            ));
            continue;
        }
        let k = std::fs::read_to_string(&kotlinc_norm)
            .unwrap()
            .replace('\r', "");
        let s = std::fs::read_to_string(&skotch_norm)
            .unwrap()
            .replace('\r', "");
        if k != s {
            failures.push(format!("{name}: divergent ({reason})"));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} aspirational parity fixture(s) diverged from kotlinc:\n  - {}\n\n\
             These fixtures lock in high-leverage Kotlin idioms. If the \
             divergence is intentional, justify it in the PR before removing \
             from tests/fixtures/kotlinc_parity_aspirational.txt.",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}
