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
pub fn try_lambda_metafactory(cf: &ClassFile, indy_idx: u16) -> Result<Option<FieldRef>> {
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
    let (captures, fi_type) = parse_descriptor(&indy_desc)?;
    if !captures.is_empty() {
        bail!("lambda: capturing lambda ({} captures) not yet supported", captures.len());
    }
    if !fi_type.starts_with('L') {
        bail!("lambda: functional-interface return {fi_type} is not a class type");
    }
    // Bootstrap static args: 0 = SAM MethodType (erased), 1 = impl MethodHandle, 2 = instantiated.
    let sam_desc = method_type(cf, bsm.arguments[0])?;
    let inst_desc = method_type(cf, bsm.arguments[2])?;
    if sam_desc != inst_desc {
        bail!("lambda: generic/bridge SAM ({sam_desc} vs {inst_desc}) not yet supported");
    }
    let (impl_kind, impl_class_internal, impl_name, impl_desc) = resolve_handle(cf, bsm.arguments[1])?;
    // reference_kind 6 = REF_invokeStatic. Only static impls (non-capturing) for now.
    if impl_kind != 6 {
        bail!("lambda: impl method-handle kind {impl_kind} (only invokestatic/6) not yet supported");
    }
    if impl_desc != sam_desc {
        bail!("lambda: impl descriptor {impl_desc} differs from SAM {sam_desc} (capturing?) not supported");
    }
    let impl_class = internal_to_descriptor(&impl_class_internal);

    // Deterministic synthetic class descriptor derived from the enclosing class + bsm slot.
    let enclosing = internal_to_descriptor(&cf.this_class); // "Lfoo/Bar;"
    let syn = format!("{}$$SkLambda${};", &enclosing[..enclosing.len() - 1], bsm_idx);
    let instance = FieldRef { class: syn.clone(), type_: fi_type.clone(), name: "INSTANCE".into() };

    PENDING.with(|p| -> Result<()> {
        if !p.borrow().contains_key(&syn) {
            let cd = build_lambda_class(&syn, &fi_type, &sam_name, &sam_desc, &impl_class, &impl_name, &impl_desc, &instance)?;
            p.borrow_mut().insert(syn.clone(), cd);
        }
        Ok(())
    })?;
    Ok(Some(instance))
}

const ACC_PUBLIC: u32 = 0x1;
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
fn build_sam(syn: &str, sam_name: &str, sam_desc: &str, impl_class: &str, impl_name: &str, impl_desc: &str) -> Result<EncodedMethod> {
    let (params, ret) = parse_descriptor(sam_desc)?;
    let mut ins: u16 = 1; // this
    let mut param_regs: Vec<u16> = Vec::new();
    let mut r = 1u16;
    for p in &params {
        if p == "J" || p == "D" {
            bail!("lambda: wide SAM parameter not yet supported");
        }
        param_regs.push(r);
        r += 1;
        ins += 1;
    }
    if param_regs.len() > 5 || param_regs.iter().any(|&x| x > 15) {
        bail!("lambda: SAM with too many parameters not yet supported");
    }
    let argn = param_regs.len() as u16;
    let g = if param_regs.len() == 5 { param_regs[4] } else { 0 };
    let mut insns: Vec<u16> = Vec::new();
    insns.push(0x71 | (((argn << 4) | g) << 8)); // invoke-static
    insns.push(0); // method-ref placeholder (fixup)
    let mut nib: u16 = 0;
    for (k, &rr) in param_regs.iter().take(4).enumerate() {
        nib |= rr << (4 * k);
    }
    insns.push(nib);
    let mut registers_size = ins;
    if ret == "V" {
        insns.push(0x000e); // return-void (10x, opcode 0x0e in the low byte)
    } else if ret == "J" || ret == "D" {
        bail!("lambda: wide SAM return not yet supported");
    } else {
        let is_ref = ret.starts_with('L') || ret.starts_with('[');
        // move-result(-object) into v0 (this, now dead), then return(-object) v0.
        insns.push(if is_ref { 0x0c } else { 0x0a });
        insns.push(if is_ref { 0x11 } else { 0x0f });
        registers_size = registers_size.max(1);
    }
    let fixups = vec![Fixup {
        unit: 1,
        item: ItemRef::Method(MethodRef { class: impl_class.into(), proto: proto(impl_desc)?, name: impl_name.into() }),
        wide: false,
    }];
    Ok(EncodedMethod {
        method: MethodRef { class: syn.into(), proto: proto(sam_desc)?, name: sam_name.into() },
        access_flags: ACC_PUBLIC,
        code: Some(CodeItem { registers_size, ins_size: ins, outs_size: argn, insns, fixups, tries: vec![], debug_info: None }),
        annotations: vec![],
    })
}

#[allow(clippy::too_many_arguments)]
fn build_lambda_class(
    syn: &str,
    fi_type: &str,
    sam_name: &str,
    sam_desc: &str,
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
    let virtual_ = vec![build_sam(syn, sam_name, sam_desc, impl_class, impl_name, impl_desc)?];
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
