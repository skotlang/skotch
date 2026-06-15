//! `invokedynamic` → `LambdaMetafactory` desugaring (Phase-2), the d8 way: a lambda is
//! lowered to a SYNTHETIC class implementing the functional interface, whose single abstract
//! method forwards to the lambda's implementation method. The `invokedynamic` call site then
//! materializes an instance of that class.
//!
//! This iteration handles the SIMPLEST shape: a NON-CAPTURING lambda (the indy has no dynamic
//! arguments) whose impl is an `invokestatic` target with a descriptor identical to the SAM
//! (no generic bridge). For these, d8 emits a SINGLETON: a `static final INSTANCE` field set
//! by the synthetic class's `<clinit>`, and the call site is just `sget-object INSTANCE`.
//! Everything else bails (never miscompiles).
//!
//! Synthetic classes are collected in a thread-local while a user class is being dexed and
//! drained by `dex_classes` into the output `DexFile`.

use crate::bootstrap::parse_descriptor;
use anyhow::{bail, Result};
use skotch_classfile::constant_pool::{internal_to_descriptor, Constant};
use skotch_classfile::model::ClassFile;
use skotch_dex::model::{
    ClassDef, CodeItem, EncodedField, EncodedMethod, FieldRef, Fixup, ItemRef, MethodRef, ProtoRef,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

thread_local! {
    /// Synthetic lambda classes generated while dexing the current class, keyed by descriptor
    /// (so a lambda referenced twice produces one class).
    static PENDING: RefCell<BTreeMap<String, ClassDef>> = const { RefCell::new(BTreeMap::new()) };
}

/// Drain and return the synthetic lambda classes generated since the last call. `dex_classes`
/// calls this after each user class is dexed and appends the result to the output.
pub fn take_pending_synthetic_classes() -> Vec<ClassDef> {
    PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()).into_values().collect())
}

fn proto(desc: &str) -> Result<ProtoRef> {
    let (params, return_type) = parse_descriptor(desc)?;
    Ok(ProtoRef { return_type, params })
}

/// The boxed wrapper descriptor for a (non-wide) primitive. Long/Double return None — we don't
/// synthesize their boxing yet (the value would occupy a register pair).
fn box_of(prim: &str) -> Option<&'static str> {
    Some(match prim {
        "I" => "Ljava/lang/Integer;",
        "Z" => "Ljava/lang/Boolean;",
        "B" => "Ljava/lang/Byte;",
        "C" => "Ljava/lang/Character;",
        "S" => "Ljava/lang/Short;",
        "F" => "Ljava/lang/Float;",
        "J" => "Ljava/lang/Long;",
        "D" => "Ljava/lang/Double;",
        _ => return None,
    })
}

/// Whether a primitive descriptor is a wide (2-register) type.
fn is_wide_prim(d: &str) -> bool {
    d == "J" || d == "D"
}

/// The conversion opcode (12x) that widens a SAM-provided primitive `from` to a WIDE impl param
/// `to` (long/double) — the only widenings whose result needs a scratch PAIR. (char/short/byte
/// arrive as int.) None if `from`→`to` isn't such a widening.
fn widen_to_wide_op(from: &str, to: &str) -> Option<u16> {
    Some(match (from, to) {
        ("I" | "C" | "S" | "B", "J") => 0x81, // int→long    (int-to-long)
        ("I" | "C" | "S" | "B", "D") => 0x83, // int→double  (int-to-double)
        ("J", "D") => 0x86,                   // long→double  (long-to-double)
        ("F", "D") => 0x89,                   // float→double (float-to-double)
        _ => return None,
    })
}

/// The (wrapper class, unboxing accessor) used to adapt a boxed SAM argument down to a primitive
/// impl parameter — the inverse of box_of. Long/double accessors return a 2-register (wide) result
/// (`move-result-wide` into a scratch pair); the caller handles their wide layout.
fn unbox_of(prim: &str) -> Option<(&'static str, &'static str)> {
    Some(match prim {
        "I" => ("Ljava/lang/Integer;", "intValue"),
        "Z" => ("Ljava/lang/Boolean;", "booleanValue"),
        "B" => ("Ljava/lang/Byte;", "byteValue"),
        "C" => ("Ljava/lang/Character;", "charValue"),
        "S" => ("Ljava/lang/Short;", "shortValue"),
        "F" => ("Ljava/lang/Float;", "floatValue"),
        "J" => ("Ljava/lang/Long;", "longValue"),
        "D" => ("Ljava/lang/Double;", "doubleValue"),
        _ => return None,
    })
}

/// How the synthetic SAM adapts the impl call's result to the SAM's declared return.
enum RetAdapt {
    /// Return the impl's result directly (same type, or a covariant reference).
    Direct,
    /// Box the impl's (non-wide) primitive into the wrapper: (boxed_descriptor, prim_descriptor).
    Box(String, String),
    /// The SAM returns void; discard the impl's result and return-void.
    DropToVoid,
}

/// Emit the synthetic SAM method's return: discard-and-return-void, or `move-result*` of the impl's
/// actual return into v0 and then either return it directly or box it via `<boxed>.valueOf(prim)`.
/// Returns `(min_registers, extra_outs)` the caller folds into the CodeItem.
fn emit_boxed_return(insns: &mut Vec<u16>, fixups: &mut Vec<Fixup>, impl_ret: &str, adapt: &RetAdapt) -> (u16, u16) {
    if matches!(adapt, RetAdapt::DropToVoid) || impl_ret == "V" {
        insns.push(0x000e); // return-void (DropToVoid ignores any impl result)
        return (0, 0);
    }
    let wide = impl_ret == "J" || impl_ret == "D";
    let is_ref = impl_ret.starts_with('L') || impl_ret.starts_with('[');
    // move-result(-wide/-object) of the impl call into v0.
    insns.push(if wide { 0x0b } else if is_ref { 0x0c } else { 0x0a });
    if let RetAdapt::Box(boxed, prim) = adapt {
        // Box the primitive (now in v0, or the pair v0,v1 for a wide long/double) via
        // invoke-static {v0[,v1]} <boxed>.valueOf(prim)boxed.
        let argn: u16 = if wide { 2 } else { 1 };
        insns.push(0x71 | (argn << 12)); // invoke-static, argn args
        let u = insns.len();
        insns.push(0);
        insns.push(if wide { 0x0010 } else { 0x0000 }); // nibbles: v0 (and vD=v1 for a wide arg)
        fixups.push(Fixup {
            unit: u,
            item: ItemRef::Method(MethodRef { class: boxed.clone(), proto: ProtoRef { return_type: boxed.clone(), params: vec![prim.clone()] }, name: "valueOf".into() }),
            wide: false,
        });
        insns.push(0x000c); // move-result-object v0
        insns.push(0x0011); // return-object v0
        (if wide { 2 } else { 1 }, argn)
    } else {
        insns.push(if wide { 0x10 } else if is_ref { 0x11 } else { 0x0f }); // return(-wide/-object) v0
        (if wide { 2 } else { 1 }, 0)
    }
}

/// Resolve a `MethodHandle` constant to `(reference_kind, internal_class, name, descriptor)`.
fn resolve_handle(cf: &ClassFile, mh_idx: u16) -> Result<(u8, String, String, String)> {
    match cf.constant_pool.get(mh_idx) {
        Constant::MethodHandle { reference_kind, reference_index } => {
            let (class, name, desc) = cf.constant_pool.member_ref(*reference_index)?;
            Ok((*reference_kind, class, name, desc))
        }
        _ => bail!("lambda: bootstrap handle index {mh_idx} is not a MethodHandle"),
    }
}

/// Resolve a `MethodType` constant to its descriptor string.
fn method_type(cf: &ClassFile, idx: u16) -> Result<String> {
    match cf.constant_pool.get(idx) {
        Constant::MethodType { descriptor_index } => Ok(cf.constant_pool.utf8(*descriptor_index)?.to_string()),
        _ => bail!("lambda: bootstrap arg {idx} is not a MethodType"),
    }
}

/// If the `invokedynamic` at constant `indy_idx` is a `LambdaMetafactory.metafactory` lambda we
/// can desugar, register the synthetic class and return the `INSTANCE` field to load at the call
/// site (the indy result). Returns `Ok(None)` if this isn't a lambda metafactory indy (caller
/// then tries other indy desugarings); bails (never miscompiles) on shapes we don't model yet.
pub fn try_lambda_metafactory(cf: &ClassFile, indy_idx: u16) -> Result<Option<LambdaSite>> {
    let (bsm_idx, nt_idx) = match cf.constant_pool.get(indy_idx) {
        Constant::InvokeDynamic { bootstrap_method_attr_index, name_and_type_index } => {
            (*bootstrap_method_attr_index, *name_and_type_index)
        }
        _ => bail!("lambda: indy constant {indy_idx} is not InvokeDynamic"),
    };
    let bsm = &cf.bootstrap_methods[bsm_idx as usize];
    let (_, bsm_class, bsm_name, _) = resolve_handle(cf, bsm.method_handle_index)?;
    // Only the plain metafactory (not altMetafactory, which carries extra flags/bridges).
    if bsm_class != "java/lang/invoke/LambdaMetafactory" || bsm_name != "metafactory" {
        return Ok(None);
    }
    if bsm.arguments.len() != 3 {
        bail!("lambda: metafactory with {} static args (expected 3) not supported", bsm.arguments.len());
    }
    // The indy NameAndType: name = the SAM method name, descriptor = (captures)FunctionalInterface.
    let (sam_name, indy_desc) = cf.constant_pool.name_and_type(nt_idx)?;
    let (sam_name, indy_desc) = (sam_name.to_string(), indy_desc.to_string());
    // The indy descriptor's parameters are the CAPTURED values; its return is the FI type.
    let (captures, fi_type) = parse_descriptor(&indy_desc)?;
    if !fi_type.starts_with('L') {
        bail!("lambda: functional-interface return {fi_type} is not a class type");
    }
    // Bootstrap static args: 0 = SAM MethodType (erased), 1 = impl MethodHandle, 2 = instantiated.
    // arg0 = the SAM MethodType the FI abstract method actually has (ERASED, e.g. (Object)Object
    // for a generic FI); arg2 = the INSTANTIATED MethodType the lambda body uses.
    let sam_desc = method_type(cf, bsm.arguments[0])?;
    let inst_desc = method_type(cf, bsm.arguments[2])?;
    let (impl_kind, impl_class_internal, impl_name, impl_desc) = resolve_handle(cf, bsm.arguments[1])?;
    // Deterministic synthetic class descriptor derived from the enclosing class + bsm slot.
    let enclosing = internal_to_descriptor(&cf.this_class); // "Lfoo/Bar;"
    let syn = format!("{}$$SkLambda${};", &enclosing[..enclosing.len() - 1], bsm_idx);

    // CONSTRUCTOR REFERENCE (kind 8 REF_newInvokeSpecial, e.g. `ArrayList::new`): the synthetic
    // SAM `new`s the class and returns it — a distinct shape from the forwarding kinds below.
    if impl_kind == 8 {
        if !captures.is_empty() {
            bail!("lambda: capturing constructor reference not yet supported");
        }
        if impl_name != "<init>" {
            bail!("lambda: newInvokeSpecial handle is not a constructor (<init>)");
        }
        let ctor_class = internal_to_descriptor(&impl_class_internal);
        let (impl_params, impl_ret) = parse_descriptor(&impl_desc)?;
        let (inst_params, inst_ret) = parse_descriptor(&inst_desc)?;
        let (sam_params, _sam_ret) = parse_descriptor(&sam_desc)?;
        if impl_ret != "V" {
            bail!("lambda: constructor handle return is not void");
        }
        // The SAM's instantiated return is the constructed class OR a supertype of it (the FI may
        // be typed more generally, e.g. `Supplier<Spliterator> = SomeSpliteratorImpl::new`).
        // return-object the new instance as the (super)type is covariantly safe — no cast.
        if !inst_ret.starts_with('L') && !inst_ret.starts_with('[') {
            bail!("lambda: ctor-ref instantiated return {inst_ret} is not a reference type");
        }
        if impl_params != inst_params {
            bail!("lambda: ctor-ref params {impl_desc} != instantiated SAM params");
        }
        let is_ref = |d: &str| d.starts_with('L') || d.starts_with('[');
        for (s, i) in sam_params.iter().zip(inst_params.iter()) {
            if s != i && !(is_ref(s) && is_ref(i)) {
                bail!("lambda: non-reference ctor-ref parameter adaptation ({s} vs {i}) not supported");
            }
        }
        let instance = FieldRef { class: syn.clone(), type_: fi_type.clone(), name: "INSTANCE".into() };
        PENDING.with(|p| -> Result<()> {
            if !p.borrow().contains_key(&syn) {
                let cd = build_ctor_class(&syn, &fi_type, &sam_name, &sam_desc, &inst_params, &ctor_class, &impl_desc, &instance)?;
                p.borrow_mut().insert(syn.clone(), cd);
            }
            Ok(())
        })?;
        return Ok(Some(LambdaSite::Singleton(instance)));
    }

    // Map the impl method-handle kind to the DEX invoke opcode the synthetic SAM forwards with.
    // 6 = invokestatic (a real lambda's static impl, OR a static method reference). 5/9/7 are
    // UNBOUND instance/interface/special method references (`String::isEmpty`): the impl is an
    // instance method, so the first instantiated SAM parameter is the receiver. 8
    // (newInvokeSpecial / constructor reference) and bound (instance-capturing) references aren't
    // modeled yet.
    let (invoke_op, instance_ref): (u16, bool) = match impl_kind {
        6 => (0x71, false), // invoke-static
        5 => (0x6e, true),  // invoke-virtual
        9 => (0x72, true),  // invoke-interface
        7 => (0x70, true),  // invoke-direct (private/super instance)
        _ => bail!("lambda: impl method-handle kind {impl_kind} not yet supported"),
    };
    let (sam_params, sam_ret) = parse_descriptor(&sam_desc)?;
    let (inst_params, inst_ret) = parse_descriptor(&inst_desc)?;
    let (impl_params, impl_ret) = parse_descriptor(&impl_desc)?;
    if sam_params.len() != inst_params.len() {
        bail!("lambda: SAM/instantiated arity mismatch ({sam_desc} vs {inst_desc})");
    }
    // A static impl takes captures ++ the instantiated SAM params. An unbound instance method
    // reference instead consumes the FIRST instantiated SAM param as the receiver (which the
    // invoke-virtual/interface/direct register list includes but the impl descriptor omits), so
    // the impl's declared params are the REMAINING instantiated SAM params and there are no
    // captures. (impl_ret must equal inst_ret exactly — a primitive/boxed mismatch would need
    // (un)boxing, which we don't synthesize → bail.)
    let expected: Vec<String> = if instance_ref {
        if captures.is_empty() {
            // UNBOUND instance ref: the receiver is the first instantiated SAM parameter; the
            // impl's declared params are the rest.
            if inst_params.is_empty() {
                bail!("lambda: instance method reference with no receiver parameter");
            }
            inst_params[1..].to_vec()
        } else {
            // The FIRST capture is the receiver (implicit in the invoke); the impl's declared
            // params are the remaining captures followed by the instantiated SAM params. This
            // covers a BOUND method reference (`sb::toString`, one capture = receiver) AND a
            // capturing lambda whose impl is an INSTANCE method (captures `this` plus locals →
            // javac emits a non-static `lambda$` impl, handle kind 5/7). The capturing path
            // synthesizes a field per capture and the SAM igets them then forwards via the
            // instance invoke_op — `invoke-virtual {f$0(receiver), f$1.., args}`.
            captures[1..].iter().cloned().chain(inst_params.iter().cloned()).collect()
        }
    } else {
        captures.iter().cloned().chain(inst_params.iter().cloned()).collect()
    };
    // Each impl parameter must match the corresponding SAM-side value, OR be a reference type the
    // value widens to (e.g. the SAM provides a String but the erased impl param is Object — passing
    // a subtype is covariantly safe, no cast). Primitive/boxed mismatches (param (un)boxing) aren't
    // modeled yet and bail.
    if impl_params.len() != expected.len() {
        bail!("lambda: impl param count {} != expected {}", impl_params.len(), expected.len());
    }
    let is_ref = |d: &str| d.starts_with('L') || d.starts_with('[');
    for (e, i) in expected.iter().zip(impl_params.iter()) {
        // e = SAM-side (instantiated) type, i = the impl's declared parameter.
        if e == i {
            continue; // exact
        }
        if is_ref(e) && is_ref(i) {
            continue; // covariant reference widening (subtype → supertype, no cast)
        }
        if box_of(i) == Some(e.as_str()) {
            continue; // param-unbox: the SAM gives a boxed value, the impl wants the primitive
        }
        if widen_to_wide_op(e, i).is_some() {
            continue; // primitive widening to a wide impl param (e.g. int→double), emitted in build_sam
        }
        bail!("lambda: impl param {i} vs SAM-side {e} adaptation (param-boxing/widening) not supported");
    }
    // Return adaptation: void SAM discards the impl result; exact returns directly; a boxed
    // instantiated return over the impl's (non-wide) primitive boxes via valueOf. Anything else
    // (widening, wide box) bails.
    let is_ref_desc = |d: &str| d.starts_with('L') || d.starts_with('[');
    let ret_adapt: RetAdapt = if inst_ret == "V" {
        RetAdapt::DropToVoid
    } else if impl_ret == inst_ret {
        RetAdapt::Direct
    } else if is_ref_desc(&impl_ret) && is_ref_desc(&inst_ret) {
        // Covariant reference return: the impl returns a SUBTYPE of the SAM's declared return (e.g.
        // ArrayList where the SAM returns List). The synthetic SAM method's return type is the
        // ERASED type (Object), so returning the impl result directly (return-object) is type-safe —
        // a subtype reference is valid wherever the supertype is expected, no cast.
        RetAdapt::Direct
    } else if box_of(&impl_ret) == Some(inst_ret.as_str()) {
        RetAdapt::Box(inst_ret.clone(), impl_ret.clone())
    } else {
        bail!("lambda: return adaptation {impl_ret} -> {inst_ret} not supported");
    };
    // Where the SAM (erased) param differs from the instantiated one, the synthetic SAM method
    // adapts by a check-cast (Object/bound → specific). Only reference→reference adaptation is a
    // plain checkcast; a primitive mismatch would need (un)boxing → bail. Same for the return
    // (a differing return must be covariant ref→ref, needing no cast since inst <: erased).
    let is_ref = |d: &str| d.starts_with('L') || d.starts_with('[');
    for (s, i) in sam_params.iter().zip(inst_params.iter()) {
        if s != i && !(is_ref(s) && is_ref(i)) {
            bail!("lambda: non-reference SAM parameter adaptation ({s} vs {i}) not supported");
        }
    }
    if sam_ret != inst_ret && !(is_ref(&sam_ret) && is_ref(&inst_ret)) {
        bail!("lambda: non-reference SAM return adaptation ({sam_ret} vs {inst_ret}) not supported");
    }
    let impl_class = internal_to_descriptor(&impl_class_internal);

    if captures.is_empty() {
        // Non-capturing: a singleton INSTANCE; the call site loads it with sget-object.
        let instance = FieldRef { class: syn.clone(), type_: fi_type.clone(), name: "INSTANCE".into() };
        PENDING.with(|p| -> Result<()> {
            if !p.borrow().contains_key(&syn) {
                // An unbound instance ref's first SAM param is the receiver (no impl-param slot),
                // so impl parameter k aligns to SAM parameter recv_offset+k.
                let recv_offset = if instance_ref { 1 } else { 0 };
                let cd = build_lambda_class(&syn, &fi_type, &sam_name, &sam_desc, &inst_params, invoke_op, recv_offset, &ret_adapt, &impl_class, &impl_name, &impl_desc, &instance)?;
                p.borrow_mut().insert(syn.clone(), cd);
            }
            Ok(())
        })?;
        Ok(Some(LambdaSite::Singleton(instance)))
    } else {
        // Capturing: one instance field per capture + a constructor taking them; the call site
        // is `new-instance + invoke-direct <init>(captures)`.
        let ctor = MethodRef { class: syn.clone(), proto: ProtoRef { return_type: "V".into(), params: captures.clone() }, name: "<init>".into() };
        PENDING.with(|p| -> Result<()> {
            if !p.borrow().contains_key(&syn) {
                let cd = build_capturing_class(&syn, &fi_type, &sam_name, &sam_desc, &inst_params, invoke_op, &ret_adapt, &impl_class, &impl_name, &impl_desc, &captures)?;
                p.borrow_mut().insert(syn.clone(), cd);
            }
            Ok(())
        })?;
        Ok(Some(LambdaSite::Capturing { class: syn, ctor, captures }))
    }
}

/// How an `invokedynamic` lambda is materialized at its call site.
pub enum LambdaSite {
    /// Non-capturing: load the synthetic class's singleton INSTANCE (`sget-object`).
    Singleton(FieldRef),
    /// Capturing: `new-instance` the synthetic class and `invoke-direct` its constructor with
    /// the captured values (popped from the operand stack, in `captures` order).
    Capturing { class: String, ctor: MethodRef, captures: Vec<String> },
}

const ACC_PUBLIC: u32 = 0x1;
const ACC_PRIVATE: u32 = 0x2;
const ACC_STATIC: u32 = 0x8;
const ACC_FINAL: u32 = 0x10;
const ACC_SYNTHETIC: u32 = 0x1000;
const ACC_CONSTRUCTOR: u32 = 0x1_0000;

/// `<init>()V`: `invoke-direct {v0}, Ljava/lang/Object;.<init>:()V ; return-void`.
fn build_init(syn: &str) -> EncodedMethod {
    // invoke-direct {v0} (35c: op 0x70 low byte, AG=0x10) ; method ; regs ; return-void (10x: 0x0e)
    let insns = vec![0x1070, 0x0000, 0x0000, 0x000e];
    let fixups = vec![Fixup {
        unit: 1,
        item: ItemRef::Method(MethodRef {
            class: "Ljava/lang/Object;".into(),
            proto: ProtoRef { return_type: "V".into(), params: vec![] },
            name: "<init>".into(),
        }),
        wide: false,
    }];
    EncodedMethod {
        method: MethodRef { class: syn.into(), proto: ProtoRef { return_type: "V".into(), params: vec![] }, name: "<init>".into() },
        access_flags: ACC_PUBLIC | ACC_CONSTRUCTOR,
        code: Some(CodeItem { registers_size: 1, ins_size: 1, outs_size: 1, insns, fixups, tries: vec![], debug_info: None }),
        annotations: vec![],
    }
}

/// `<clinit>()V`: `new-instance v0, syn ; invoke-direct {v0}, syn.<init>()V ; sput-object v0,
/// syn.INSTANCE ; return-void` — d8's non-capturing singleton.
fn build_clinit(syn: &str, instance: &FieldRef) -> EncodedMethod {
    // new-instance v0 (21c: op 0x22, AA=v0=0) ; type ; invoke-direct {v0} <init> ; method ;
    // regs ; sput-object v0 (21c: op 0x69) ; field ; return-void. Opcodes are the LOW byte.
    let insns = vec![0x0022, 0, 0x1070, 0, 0x0000, 0x0069, 0, 0x000e];
    let fixups = vec![
        Fixup { unit: 1, item: ItemRef::Type(syn.into()), wide: false },
        Fixup {
            unit: 3,
            item: ItemRef::Method(MethodRef { class: syn.into(), proto: ProtoRef { return_type: "V".into(), params: vec![] }, name: "<init>".into() }),
            wide: false,
        },
        Fixup { unit: 6, item: ItemRef::Field(instance.clone()), wide: false },
    ];
    EncodedMethod {
        method: MethodRef { class: syn.into(), proto: ProtoRef { return_type: "V".into(), params: vec![] }, name: "<clinit>".into() },
        access_flags: ACC_STATIC | ACC_CONSTRUCTOR,
        code: Some(CodeItem { registers_size: 1, ins_size: 0, outs_size: 1, insns, fixups, tries: vec![], debug_info: None }),
        annotations: vec![],
    }
}

/// The SAM method: forwards its arguments to the (static) impl method and returns. Register
/// layout: `this` is v0, the SAM parameters follow at v1.. (no captures). The impl takes exactly
/// the SAM parameters (verified by the caller), so we `invoke-static {param regs}, impl`.
#[allow(clippy::too_many_arguments)]
fn build_sam(syn: &str, sam_name: &str, sam_desc: &str, inst_params: &[String], invoke_op: u16, recv_offset: usize, ret_adapt: &RetAdapt, impl_class: &str, impl_name: &str, impl_desc: &str) -> Result<EncodedMethod> {
    let (params, _ret) = parse_descriptor(sam_desc)?;
    let (impl_p, _impl_ret0) = parse_descriptor(impl_desc)?;
    // A wide (long/double) impl param produced from a narrower SAM value — either UNBOXED from a
    // boxed wrapper, or WIDENED from a smaller primitive (e.g. int→double) — lands in a 2-register
    // PAIR that can't sit in the 1-slot source. It goes to a scratch pair; DEX places the `ins`
    // incoming params in the HIGH registers, so the scratch goes at the LOW registers (v0..) and the
    // params shift ABOVE it. `widen_conv[k]` is Some(conversion-opcode) for a widen, None for an
    // unbox. (With no wide adaptation, scratch_words == 0 and the layout is unchanged.)
    let mut wide_scratch: Vec<Option<u16>> = vec![None; params.len()];
    let mut widen_conv: Vec<Option<u16>> = vec![None; params.len()];
    let mut scratch_words = 0u16;
    for k in 0..params.len() {
        if k < recv_offset {
            continue; // receiver (unbound instance ref) has no impl-param slot
        }
        let ii = k - recv_offset;
        if ii >= impl_p.len() || !is_wide_prim(&impl_p[ii]) {
            continue;
        }
        if box_of(&impl_p[ii]) == Some(inst_params[k].as_str()) {
            wide_scratch[k] = Some(scratch_words);
            scratch_words += 2;
        } else if let Some(op) = widen_to_wide_op(&inst_params[k], &impl_p[ii]) {
            wide_scratch[k] = Some(scratch_words);
            widen_conv[k] = Some(op);
            scratch_words += 2;
        }
    }
    let mut ins: u16 = 1; // this
    // First register + wide flag per SAM parameter (a long/double occupies the pair r, r+1).
    let mut param_start: Vec<u16> = Vec::new();
    let mut param_wide: Vec<bool> = Vec::new();
    let mut r = scratch_words + 1; // params sit above the low scratch (this = v[scratch_words])
    for p in &params {
        let wide = p == "J" || p == "D";
        param_start.push(r);
        param_wide.push(wide);
        let w = if wide { 2 } else { 1 };
        r += w;
        ins += w;
    }
    // The flat register list the impl invoke passes — a wide arg lists BOTH halves consecutively;
    // a wide-adapted (unbox/widen) param lists its scratch pair.
    let mut invoke_regs: Vec<u16> = Vec::new();
    for (i, &st) in param_start.iter().enumerate() {
        if let Some(sc) = wide_scratch[i] {
            invoke_regs.push(sc);
            invoke_regs.push(sc + 1);
        } else {
            invoke_regs.push(st);
            if param_wide[i] {
                invoke_regs.push(st + 1);
            }
        }
    }
    if invoke_regs.len() > 5 || invoke_regs.iter().any(|&x| x > 15) {
        bail!("lambda: SAM invoke needs range form (too many/high register words) not yet supported");
    }
    let mut insns: Vec<u16> = Vec::new();
    let mut fixups: Vec<Fixup> = Vec::new();
    // Adapt each erased reference parameter to its instantiated type (in-place check-cast). Wide
    // primitive params never need this (erased == instantiated == J/D).
    for (k, (s, i)) in params.iter().zip(inst_params.iter()).enumerate() {
        if s != i && !param_wide[k] {
            insns.push(0x1f | (param_start[k] << 8)); // check-cast vReg, InstType (21c)
            let unit = insns.len();
            insns.push(0);
            fixups.push(Fixup { unit, item: ItemRef::Type(i.clone()), wide: false });
        }
    }
    // Adapt each argument the impl wants as a primitive. impl declared parameter k maps to SAM
    // parameter recv_offset+k. A WIDEN (e.g. int→double) emits a single 12x conversion into the
    // scratch pair. An UNBOX invoke-virtuals the accessor: a wide result goes to the scratch pair,
    // a narrow one unboxes in place at the param register.
    let mut did_unbox = false;
    for (k, ip) in impl_p.iter().enumerate() {
        let sam_idx = recv_offset + k;
        if sam_idx >= inst_params.len() {
            break;
        }
        if let Some(op) = widen_conv[sam_idx] {
            // <conv> vScratch, vParam (12x) — e.g. `int-to-double v0, v3`.
            let sc = wide_scratch[sam_idx].expect("widen implies a scratch pair");
            if sc > 15 || param_start[sam_idx] > 15 {
                bail!("lambda: widen conversion register out of nibble range");
            }
            insns.push(op | (sc << 8) | (param_start[sam_idx] << 12));
            continue;
        }
        if box_of(ip) != Some(inst_params[sam_idx].as_str()) {
            continue; // not a boxed→primitive position
        }
        let (bx, m) = unbox_of(ip).ok_or_else(|| anyhow::anyhow!("lambda: no unbox accessor for {ip}"))?;
        // invoke-virtual {boxedReg} <Wrapper>.<accessor>()prim
        insns.push(0x6e | ((1u16 << 4) << 8)); // invoke-virtual, argn=1 → 0x106e
        let unit = insns.len();
        insns.push(0);
        insns.push(param_start[sam_idx]); // 35c arg nibble vC = the boxed receiver
        fixups.push(Fixup {
            unit,
            item: ItemRef::Method(MethodRef { class: bx.into(), proto: ProtoRef { return_type: ip.clone(), params: vec![] }, name: m.into() }),
            wide: false,
        });
        if let Some(sc) = wide_scratch[sam_idx] {
            insns.push(0x0b | (sc << 8)); // move-result-wide scratch pair (low registers)
        } else {
            insns.push(0x0a | (param_start[sam_idx] << 8)); // move-result paramReg (in place)
        }
        did_unbox = true;
    }
    // invoke {param regs}, impl — invoke-static for a static impl/method-ref, or
    // invoke-virtual/interface/direct for an unbound instance method reference (arg0 = receiver).
    let argn = invoke_regs.len() as u16;
    let g = if invoke_regs.len() == 5 { invoke_regs[4] } else { 0 };
    insns.push(invoke_op | (((argn << 4) | g) << 8));
    let munit = insns.len();
    insns.push(0); // method-ref placeholder (fixup)
    let mut nib: u16 = 0;
    for (k, &rr) in invoke_regs.iter().take(4).enumerate() {
        nib |= rr << (4 * k);
    }
    insns.push(nib);
    fixups.push(Fixup { unit: munit, item: ItemRef::Method(MethodRef { class: impl_class.into(), proto: proto(impl_desc)?, name: impl_name.into() }), wide: false });
    // Return the impl's actual result into v0 (this, now dead), boxing it if the SAM return is a
    // boxed wrapper of the impl's primitive.
    let (_, impl_ret) = parse_descriptor(impl_desc)?;
    let (min_regs, extra_outs) = emit_boxed_return(&mut insns, &mut fixups, &impl_ret, ret_adapt);
    // ins params (high) + the low scratch pairs; min_regs covers a wide return-box needing v0,v1.
    let registers_size = (ins + scratch_words).max(min_regs);
    let unbox_outs = if did_unbox { 1 } else { 0 };
    Ok(EncodedMethod {
        method: MethodRef { class: syn.into(), proto: proto(sam_desc)?, name: sam_name.into() },
        access_flags: ACC_PUBLIC,
        code: Some(CodeItem { registers_size, ins_size: ins, outs_size: argn.max(extra_outs).max(unbox_outs), insns, fixups, tries: vec![], debug_info: None }),
        annotations: vec![],
    })
}

#[allow(clippy::too_many_arguments)]
fn build_lambda_class(
    syn: &str,
    fi_type: &str,
    sam_name: &str,
    sam_desc: &str,
    inst_params: &[String],
    invoke_op: u16,
    recv_offset: usize,
    ret_adapt: &RetAdapt,
    impl_class: &str,
    impl_name: &str,
    impl_desc: &str,
    instance: &FieldRef,
) -> Result<ClassDef> {
    let instance_field = EncodedField {
        field: instance.clone(),
        access_flags: ACC_PUBLIC | ACC_STATIC | ACC_FINAL | ACC_SYNTHETIC,
        annotations: vec![],
    };
    // Direct methods must be encoded ascending by method index (name then proto); "<clinit>" <
    // "<init>" lexicographically, so this order is always correct.
    let direct = vec![build_clinit(syn, instance), build_init(syn)];
    let virtual_ = vec![build_sam(syn, sam_name, sam_desc, inst_params, invoke_op, recv_offset, ret_adapt, impl_class, impl_name, impl_desc)?];
    Ok(ClassDef {
        class_type: syn.into(),
        access_flags: ACC_PUBLIC | ACC_FINAL | ACC_SYNTHETIC,
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![fi_type.into()],
        source_file: None,
        static_fields: vec![instance_field],
        instance_fields: vec![],
        direct_methods: direct,
        virtual_methods: virtual_,
        static_values: vec![],
        annotations: vec![],
    })
}

// ──────────────────────────── constructor references ───────────────────────

/// The SAM method for a constructor reference (`Foo::new`): `new-instance v0, Foo` ;
/// `invoke-direct {v0, args}, Foo.<init>(args)V` ; `return-object v0`. The result IS the new
/// object (no impl move-result). v0 is `this` (the singleton, dead) reused for the new instance;
/// the SAM parameters (cast to the constructor's parameter types) follow at v1.. .
fn build_ctor_sam(syn: &str, sam_name: &str, sam_desc: &str, inst_params: &[String], ctor_class: &str, ctor_desc: &str) -> Result<EncodedMethod> {
    let (params, _ret) = parse_descriptor(sam_desc)?;
    let mut ins: u16 = 1; // this (v0, reused for the new object after entry)
    // First register + wide flag per SAM param (a long/double occupies the pair r, r+1).
    let mut param_start: Vec<u16> = Vec::new();
    let mut param_wide: Vec<bool> = Vec::new();
    let mut r = 1u16;
    for p in &params {
        let wide = p == "J" || p == "D";
        param_start.push(r);
        param_wide.push(wide);
        let w = if wide { 2 } else { 1 };
        r += w;
        ins += w;
    }
    // invoke-direct passes the new object (v0) plus the params (a wide param lists BOTH halves).
    let mut invoke_regs: Vec<u16> = vec![0];
    for (i, &st) in param_start.iter().enumerate() {
        invoke_regs.push(st);
        if param_wide[i] {
            invoke_regs.push(st + 1);
        }
    }
    if invoke_regs.len() > 5 || invoke_regs.iter().any(|&x| x > 15) {
        bail!("lambda: constructor reference with too many parameters not yet supported");
    }
    let mut insns: Vec<u16> = Vec::new();
    let mut fixups: Vec<Fixup> = Vec::new();
    // Adapt each erased reference parameter to the constructor's parameter type (check-cast). A wide
    // primitive param never needs this (erased == instantiated == J/D).
    for (k, (s, i)) in params.iter().zip(inst_params.iter()).enumerate() {
        if s != i && !param_wide[k] {
            insns.push(0x1f | (param_start[k] << 8));
            let unit = insns.len();
            insns.push(0);
            fixups.push(Fixup { unit, item: ItemRef::Type(i.clone()), wide: false });
        }
    }
    // new-instance v0, ctor_class (21c: op 0x22 low byte, AA=v0=0).
    insns.push(0x0022);
    let nu = insns.len();
    insns.push(0);
    fixups.push(Fixup { unit: nu, item: ItemRef::Type(ctor_class.into()), wide: false });
    // invoke-direct {v0(new), params..}, ctor_class.<init>(params)V.
    let argn = invoke_regs.len() as u16;
    let g = if invoke_regs.len() == 5 { invoke_regs[4] } else { 0 };
    insns.push(0x70 | (((argn << 4) | g) << 8));
    let mu = insns.len();
    insns.push(0);
    let mut nib: u16 = 0;
    for (k, &rr) in invoke_regs.iter().take(4).enumerate() {
        nib |= rr << (4 * k);
    }
    insns.push(nib);
    fixups.push(Fixup { unit: mu, item: ItemRef::Method(MethodRef { class: ctor_class.into(), proto: proto(ctor_desc)?, name: "<init>".into() }), wide: false });
    insns.push(0x0011); // return-object v0
    Ok(EncodedMethod {
        method: MethodRef { class: syn.into(), proto: proto(sam_desc)?, name: sam_name.into() },
        access_flags: ACC_PUBLIC,
        code: Some(CodeItem { registers_size: ins, ins_size: ins, outs_size: argn, insns, fixups, tries: vec![], debug_info: None }),
        annotations: vec![],
    })
}

/// A non-capturing constructor reference's synthetic class: a singleton (like build_lambda_class)
/// whose SAM is build_ctor_sam.
#[allow(clippy::too_many_arguments)]
fn build_ctor_class(syn: &str, fi_type: &str, sam_name: &str, sam_desc: &str, inst_params: &[String], ctor_class: &str, ctor_desc: &str, instance: &FieldRef) -> Result<ClassDef> {
    let instance_field = EncodedField {
        field: instance.clone(),
        access_flags: ACC_PUBLIC | ACC_STATIC | ACC_FINAL | ACC_SYNTHETIC,
        annotations: vec![],
    };
    let direct = vec![build_clinit(syn, instance), build_init(syn)];
    let virtual_ = vec![build_ctor_sam(syn, sam_name, sam_desc, inst_params, ctor_class, ctor_desc)?];
    Ok(ClassDef {
        class_type: syn.into(),
        access_flags: ACC_PUBLIC | ACC_FINAL | ACC_SYNTHETIC,
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![fi_type.into()],
        source_file: None,
        static_fields: vec![instance_field],
        instance_fields: vec![],
        direct_methods: direct,
        virtual_methods: virtual_,
        static_values: vec![],
        annotations: vec![],
    })
}

// ──────────────────────────── capturing lambdas ────────────────────────────

/// `<init>(captures)V`: `invoke-direct {v0}, Object.<init>()V` then `iput vN, v0, f$(N-1)` for
/// each capture, `return-void`. Register layout: `this` = v0, capture args = v1.. (the ins).
fn build_capturing_init(syn: &str, captures: &[String]) -> Result<EncodedMethod> {
    // Per-capture starting register after `this` (v0); a wide (long/double) capture occupies a pair.
    let cap_width = |ty: &str| -> u16 { if ty == "J" || ty == "D" { 2 } else { 1 } };
    let total: u16 = captures.iter().map(|t| cap_width(t)).sum();
    let regs = 1 + total; // this + capture params
    // iget/iput nibble (22c) caps the object (v0) and value at v15; bail if a capture lands ≥16.
    if regs > 16 {
        bail!("lambda: capture register block needs {regs} registers (>16) not supported");
    }
    let mut insns: Vec<u16> = vec![0x1070, 0, 0x0000]; // invoke-direct {v0} Object.<init>
    let mut fixups = vec![Fixup {
        unit: 1,
        item: ItemRef::Method(MethodRef { class: "Ljava/lang/Object;".into(), proto: ProtoRef { return_type: "V".into(), params: vec![] }, name: "<init>".into() }),
        wide: false,
    }];
    let mut valreg = 1u16; // v1.. (after this)
    for (i, ty) in captures.iter().enumerate() {
        // iput*(-wide) valreg, v0, f$i (22c: op low byte, value in bits 8-11, object v0 in bits 12-15).
        insns.push(crate::bootstrap::iput_op(ty) | (valreg << 8));
        let unit = insns.len();
        insns.push(0);
        fixups.push(Fixup { unit, item: ItemRef::Field(FieldRef { class: syn.into(), type_: ty.clone(), name: format!("f${i}") }), wide: false });
        valreg += cap_width(ty);
    }
    insns.push(0x000e); // return-void
    Ok(EncodedMethod {
        method: MethodRef { class: syn.into(), proto: ProtoRef { return_type: "V".into(), params: captures.to_vec() }, name: "<init>".into() },
        access_flags: ACC_PUBLIC | ACC_CONSTRUCTOR,
        code: Some(CodeItem { registers_size: regs, ins_size: regs, outs_size: 1, insns, fixups, tries: vec![], debug_info: None }),
        annotations: vec![],
    })
}

/// The SAM method for a capturing lambda: load each `this.f$N` capture, then `invoke-static` the
/// impl with `[captures.., sam params]` and return. Register layout: captures load into v0..v(c-1);
/// `this` is at vc; the SAM parameters are the ins at v(c+1)... .
fn build_capturing_sam(syn: &str, sam_name: &str, sam_desc: &str, inst_params: &[String], invoke_op: u16, ret_adapt: &RetAdapt, impl_class: &str, impl_name: &str, impl_desc: &str, captures: &[String]) -> Result<EncodedMethod> {
    let (sam_params, _ret) = parse_descriptor(sam_desc)?;
    let cap_width = |ty: &str| -> u16 { if ty == "J" || ty == "D" { 2 } else { 1 } };
    // Per-capture starting register; captures load into the LOW registers v0.. (a wide capture
    // occupies a consecutive pair). `this` follows them, then the SAM parameters.
    let mut cap_start: Vec<u16> = Vec::with_capacity(captures.len());
    let mut total_cap = 0u16;
    for ty in captures {
        cap_start.push(total_cap);
        total_cap += cap_width(ty);
    }
    let p = sam_params.len();
    // Per-SAM-param starting word offset (a wide long/double param occupies a consecutive PAIR), so
    // the param registers and the impl-invoke marshalling account for width, not just count.
    let mut param_start: Vec<u16> = Vec::with_capacity(p);
    let mut param_words = 0u16;
    for ty in &sam_params {
        param_start.push(param_words);
        param_words += cap_width(ty);
    }
    // The impl's params are [captures.. , sam params], so SAM param k maps to impl parameter
    // `off + k` (the captures that precede them — except a bound-receiver capture, which is the
    // invoke receiver and not in impl_desc, leaving off == 0).
    let (impl_p, _impl_ret0) = parse_descriptor(impl_desc)?;
    let off = impl_p.len().saturating_sub(inst_params.len());
    // A boxed Long/Double SAM param the impl wants as a primitive long/double unboxes into a scratch
    // PAIR (1 ref slot → 2-slot pair). DEX puts the `ins` params (this + sam params) in the HIGH
    // registers, so the scratch goes at the LOW registers just above the captures, and `this`/params
    // shift up by `scratch_words`. (No wide-unbox → scratch_words == 0 → layout unchanged.)
    let mut unbox_scratch: Vec<Option<u16>> = vec![None; p];
    let mut scratch_words = 0u16;
    for k in 0..inst_params.len() {
        let ip = &impl_p[off + k];
        if is_wide_prim(ip) && box_of(ip) == Some(inst_params[k].as_str()) {
            unbox_scratch[k] = Some(total_cap + scratch_words);
            scratch_words += 2;
        }
    }
    let regs = total_cap + scratch_words + 1 + param_words; // captures + scratch + this + sam params
    if regs > 16 {
        bail!("lambda: capturing SAM needs {regs} registers (>16) not supported");
    }
    let this_reg = total_cap + scratch_words;
    // reg-WORDS passed to the impl: captures (each its width) + each impl param's width (a wide-unbox
    // param contributes the 2-word primitive, not the 1-word boxed ref).
    let argn = total_cap + (0..inst_params.len()).map(|k| cap_width(&impl_p[off + k])).sum::<u16>();
    if argn > 5 {
        bail!("lambda: capturing SAM impl has {argn} arg words (>5, needs range form) not supported");
    }
    let mut insns: Vec<u16> = Vec::new();
    let mut fixups: Vec<Fixup> = Vec::new();
    // Load captures: iget*(-wide) v(cap_start), this, f$i (a wide value lands in cap_start, cap_start+1).
    for (i, ty) in captures.iter().enumerate() {
        insns.push(crate::bootstrap::iget_op(ty) | (cap_start[i] << 8) | (this_reg << 12));
        let unit = insns.len();
        insns.push(0);
        fixups.push(Fixup { unit, item: ItemRef::Field(FieldRef { class: syn.into(), type_: ty.clone(), name: format!("f${i}") }), wide: false });
    }
    // Adapt each erased reference SAM parameter (at v(c+1)..) to its instantiated type in place.
    for (k, (s, i)) in sam_params.iter().zip(inst_params.iter()).enumerate() {
        if s != i {
            let reg = this_reg + 1 + param_start[k];
            insns.push(0x1f | (reg << 8)); // check-cast vReg, InstType (21c)
            let unit = insns.len();
            insns.push(0);
            fixups.push(Fixup { unit, item: ItemRef::Type(i.clone()), wide: false });
        }
    }
    // Unbox each boxed SAM parameter the impl wants as a primitive. A wide (long/double) unbox
    // writes its 2-register result into the reserved scratch pair; a narrow one unboxes in place.
    let mut did_unbox = false;
    for (k, sam_ty) in inst_params.iter().enumerate() {
        let ip = &impl_p[off + k];
        if box_of(ip) != Some(sam_ty.as_str()) {
            continue; // not a boxed→primitive position
        }
        let (bx, m) = unbox_of(ip).ok_or_else(|| anyhow::anyhow!("lambda: no unbox accessor for {ip}"))?;
        let reg = this_reg + 1 + param_start[k];
        // invoke-virtual {boxedReg} <Wrapper>.<accessor>()prim
        insns.push(0x6e | ((1u16 << 4) << 8)); // invoke-virtual, argn=1 → 0x106e
        let unit = insns.len();
        insns.push(0);
        insns.push(reg); // 35c arg nibble vC = the boxed receiver
        fixups.push(Fixup {
            unit,
            item: ItemRef::Method(MethodRef { class: bx.into(), proto: ProtoRef { return_type: ip.clone(), params: vec![] }, name: m.into() }),
            wide: false,
        });
        if let Some(sc) = unbox_scratch[k] {
            insns.push(0x0b | (sc << 8)); // move-result-wide scratch pair (low registers)
        } else {
            insns.push(0x0a | (reg << 8)); // move-result reg (in place)
        }
        did_unbox = true;
    }
    // invoke {captures.. (both halves of a wide), sam params}, impl.
    let mut arg_regs: Vec<u16> = Vec::new();
    for (i, ty) in captures.iter().enumerate() {
        arg_regs.push(cap_start[i]);
        if cap_width(ty) == 2 {
            arg_regs.push(cap_start[i] + 1);
        }
    }
    for (k, ty) in sam_params.iter().enumerate() {
        if let Some(sc) = unbox_scratch[k] {
            arg_regs.push(sc); // the unboxed primitive's scratch pair (low registers)
            arg_regs.push(sc + 1);
        } else {
            let reg = this_reg + 1 + param_start[k];
            arg_regs.push(reg);
            if cap_width(ty) == 2 {
                arg_regs.push(reg + 1);
            }
        }
    }
    let g = if arg_regs.len() == 5 { arg_regs[4] } else { 0 };
    insns.push(invoke_op | (((argn << 4) | g) << 8));
    let munit = insns.len();
    insns.push(0);
    let mut nib: u16 = 0;
    for (k, &rr) in arg_regs.iter().take(4).enumerate() {
        nib |= rr << (4 * k);
    }
    insns.push(nib);
    fixups.push(Fixup {
        unit: munit,
        item: ItemRef::Method(MethodRef { class: impl_class.into(), proto: proto(impl_desc)?, name: impl_name.into() }),
        wide: false,
    });
    // Return the impl's actual result into v0 (capture 0, now dead), boxing if the SAM return is a
    // boxed wrapper of the impl's primitive.
    let (_, impl_ret) = parse_descriptor(impl_desc)?;
    let (_min_regs, extra_outs) = emit_boxed_return(&mut insns, &mut fixups, &impl_ret, ret_adapt);
    let ins_size = 1 + param_words; // this + sam params (a wide param is 2 regs)
    Ok(EncodedMethod {
        method: MethodRef { class: syn.into(), proto: proto(sam_desc)?, name: sam_name.into() },
        access_flags: ACC_PUBLIC,
        code: Some(CodeItem { registers_size: regs, ins_size, outs_size: argn.max(extra_outs).max(u16::from(did_unbox)), insns, fixups, tries: vec![], debug_info: None }),
        annotations: vec![],
    })
}

#[allow(clippy::too_many_arguments)]
fn build_capturing_class(syn: &str, fi_type: &str, sam_name: &str, sam_desc: &str, inst_params: &[String], invoke_op: u16, ret_adapt: &RetAdapt, impl_class: &str, impl_name: &str, impl_desc: &str, captures: &[String]) -> Result<ClassDef> {
    // One private-final field per capture. Encoded order must ascend by field name; sort to be
    // safe (f$0..f$9 already ascend lexicographically, but f$10+ would not).
    let mut fields: Vec<EncodedField> = captures
        .iter()
        .enumerate()
        .map(|(i, ty)| EncodedField {
            field: FieldRef { class: syn.into(), type_: ty.clone(), name: format!("f${i}") },
            access_flags: ACC_PRIVATE | ACC_FINAL | ACC_SYNTHETIC,
            annotations: vec![],
        })
        .collect();
    fields.sort_by(|a, b| a.field.name.cmp(&b.field.name));
    let ctor = build_capturing_init(syn, captures)?;
    let sam = build_capturing_sam(syn, sam_name, sam_desc, inst_params, invoke_op, ret_adapt, impl_class, impl_name, impl_desc, captures)?;
    Ok(ClassDef {
        class_type: syn.into(),
        access_flags: ACC_PUBLIC | ACC_FINAL | ACC_SYNTHETIC,
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![fi_type.into()],
        source_file: None,
        static_fields: vec![],
        instance_fields: fields,
        direct_methods: vec![ctor],
        virtual_methods: vec![sam],
        static_values: vec![],
        annotations: vec![],
    })
}
