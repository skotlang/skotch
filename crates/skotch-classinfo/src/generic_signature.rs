//! Parser for JVM `Signature` attribute strings (JVMS §4.7.9.1).
//!
//! The Signature attribute is the unerased view of a generic class,
//! method, or field. Examples (from `kotlin-stdlib.jar`):
//!
//! * `<T:Ljava/lang/Object;>([TT;)Ljava/util/List<TT;>;`
//!   — `<T> fun listOf(vararg elements: T): List<T>`
//! * `<T:Ljava/lang/Object;R:Ljava/lang/Object;>(Lkotlin/jvm/functions/Function1<-TT;+TR;>;)TR;`
//!   — `<T, R> fun let(block: (T) -> R): R` (the receiver is implicit)
//! * `Ljava/util/List<Lcom/example/Message;>;` (field signature)
//!   — `val xs: List<Message>`
//!
//! Skotch reads this attribute to recover type-variable information
//! from the classpath, so generic stdlib functions can be inferred
//! without enumerating their names. The parser returns a structured
//! [`MethodSignature`] / [`JavaTypeSignature`] tree; the unifier
//! and substitution functions live alongside as `unify` / `subst`.

/// A method's full generic signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodSignature {
    /// Type parameters introduced by this method (or the enclosing
    /// class — for our purposes they're treated uniformly).
    pub type_params: Vec<TypeParam>,
    /// Formal parameter types in declaration order. For an instance
    /// method, this does NOT include the implicit receiver (the JVM
    /// descriptor doesn't either).
    pub param_tys: Vec<JavaTypeSignature>,
    /// Return type. `V` (void) becomes `Primitive('V')`.
    pub return_ty: JavaTypeSignature,
}

/// A formal type parameter declaration: `<T: Bound>` becomes
/// `TypeParam { name: "T", upper_bounds: vec![Bound] }`. Bounds are
/// parsed but currently unused by the inferrer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeParam {
    pub name: String,
    pub upper_bounds: Vec<JavaTypeSignature>,
}

/// One Java type signature, as a single value (not a method).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JavaTypeSignature {
    /// JVM primitive: `B`, `S`, `I`, `J`, `F`, `D`, `Z`, `C`, or `V`
    /// (void — return position only).
    Primitive(char),
    /// `Lpkg/Class<args>;` — fully qualified internal class name plus
    /// optional generic args. Inner classes can chain via `.Inner`,
    /// which we flatten into the `name` (e.g. `pkg/Outer$Inner`)
    /// and re-attach to the outer args via `[$]`.
    ClassType {
        name: String,
        args: Vec<JavaTypeArg>,
    },
    /// `TT;` — reference to a type variable by name.
    TypeVar(String),
    /// `[X` — array of X.
    Array(Box<JavaTypeSignature>),
}

/// One generic argument inside a `ClassType<...>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JavaTypeArg {
    /// Concrete invariant arg: `<Foo>`.
    Type(JavaTypeSignature),
    /// `<? extends Foo>` — covariant bound.
    ExtendsBound(JavaTypeSignature),
    /// `<? super Foo>` — contravariant bound.
    SuperBound(JavaTypeSignature),
    /// `<*>` — star projection.
    Star,
}

impl JavaTypeArg {
    /// The contained type, ignoring variance markers. `<*>` returns `None`.
    pub fn as_type(&self) -> Option<&JavaTypeSignature> {
        match self {
            JavaTypeArg::Type(t) | JavaTypeArg::ExtendsBound(t) | JavaTypeArg::SuperBound(t) => {
                Some(t)
            }
            JavaTypeArg::Star => None,
        }
    }
}

impl JavaTypeSignature {
    /// JVM-internal class name underlying this type, if any. For
    /// `List<T>` returns `"java/util/List"`. For type variables and
    /// primitives returns `None`.
    pub fn class_name(&self) -> Option<&str> {
        match self {
            JavaTypeSignature::ClassType { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    /// Type arguments if this is a parameterized class type.
    pub fn type_args(&self) -> &[JavaTypeArg] {
        match self {
            JavaTypeSignature::ClassType { args, .. } => args.as_slice(),
            _ => &[],
        }
    }
}

/// Result of unifying a generic call's actual arg types against the
/// method's formal signature. Maps each bound type variable to the
/// concrete `Ty` that satisfies it.
pub type Substitution = std::collections::HashMap<String, skotch_types::Ty>;

/// Bridge a `JavaTypeSignature` into skotch's `Ty` lattice, applying
/// the given substitution to any type-variable references. Unknown
/// type variables become `Ty::Any` (the formal was generic but no
/// constraint pinned it).
pub fn signature_to_ty(sig: &JavaTypeSignature, subst: &Substitution) -> skotch_types::Ty {
    use skotch_types::Ty;
    match sig {
        JavaTypeSignature::Primitive(c) => match c {
            'V' => Ty::Unit,
            'Z' => Ty::Bool,
            'B' => Ty::Byte,
            'S' => Ty::Short,
            'C' => Ty::Char,
            'I' => Ty::Int,
            'J' => Ty::Long,
            'F' => Ty::Float,
            'D' => Ty::Double,
            _ => Ty::Any,
        },
        JavaTypeSignature::TypeVar(name) => subst.get(name).cloned().unwrap_or(Ty::Any),
        JavaTypeSignature::Array(inner) => {
            // Map to the corresponding primitive array type when possible.
            match inner.as_ref() {
                JavaTypeSignature::Primitive('I') => Ty::IntArray,
                JavaTypeSignature::Primitive('J') => Ty::LongArray,
                JavaTypeSignature::Primitive('D') => Ty::DoubleArray,
                JavaTypeSignature::Primitive('Z') => Ty::BooleanArray,
                JavaTypeSignature::Primitive('B') => Ty::ByteArray,
                // Object arrays don't have a dedicated Ty variant
                // today — surface them as Any so the descriptor
                // builder writes `[Ljava/lang/Object;`.
                _ => Ty::Any,
            }
        }
        JavaTypeSignature::ClassType { name, args } => {
            // Promote a few well-known classes to skotch's primitive-
            // ish Ty variants so downstream code doesn't have to
            // pattern-match on string class names.
            let class_ty = match name.as_str() {
                "java/lang/String" => Ty::String,
                _ => Ty::Class(name.clone()),
            };
            if args.is_empty() {
                class_ty
            } else {
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .map(|a| match a.as_type() {
                        Some(inner) => signature_to_ty(inner, subst),
                        None => Ty::Any,
                    })
                    .collect();
                Ty::Generic {
                    base: Box::new(class_ty),
                    args: arg_tys,
                }
            }
        }
    }
}

/// Unify a single formal `JavaTypeSignature` against an actual
/// `Ty`, accumulating type-variable bindings into `subst`. Best-
/// effort: pinned bindings stay (we don't run a full constraint
/// solver), so the FIRST occurrence of a type variable wins. This is
/// adequate for typical Kotlin signatures where T appears once and
/// determines the rest.
pub fn unify_one(formal: &JavaTypeSignature, actual: &skotch_types::Ty, subst: &mut Substitution) {
    use skotch_types::Ty;
    // `Nullable(X)` unifies the same as `X` — the nullable wrapper
    // doesn't constrain the underlying type variable.
    if let Ty::Nullable(inner) = actual {
        unify_one(formal, inner, subst);
        return;
    }
    match formal {
        JavaTypeSignature::TypeVar(name) => {
            // Skip vacuous bindings — Any/Error/Unit don't help.
            if !matches!(actual, Ty::Any | Ty::Error | Ty::Unit | Ty::Nothing)
                && !subst.contains_key(name)
            {
                subst.insert(name.clone(), actual.clone());
            }
        }
        JavaTypeSignature::Array(inner_sig) => {
            // Without a generic Array(T) variant we can't recurse —
            // primitive arrays carry no element type information.
            // Object arrays would need the actual arg to be a list-
            // like Ty::Generic too; rare in practice.
            let _ = inner_sig;
        }
        JavaTypeSignature::ClassType { name, args } => {
            // For Function-typed formals (`Function1<-T, +R>`),
            // pluck T from the actual lambda's param types and R
            // from its return type. The signatures use
            // `kotlin.jvm.functions.FunctionN` for arity N.
            if name.starts_with("kotlin/jvm/functions/Function") {
                if let Ty::Function { params, ret, .. } = actual {
                    // Last arg slot in Function signatures is the
                    // return type; preceding are param types in
                    // order. e.g. `Function1<-T, +R>` has args
                    // [T, R].
                    if !args.is_empty() {
                        let last = args.len() - 1;
                        if let Some(ret_sig) = args[last].as_type() {
                            unify_one(ret_sig, ret, subst);
                        }
                        for (i, param_ty) in params.iter().enumerate() {
                            if i < last {
                                if let Some(param_sig) = args[i].as_type() {
                                    unify_one(param_sig, param_ty, subst);
                                }
                            }
                        }
                    }
                }
                return;
            }
            // Generic class unification: line up our recorded
            // generic args with the formal's.
            let actual_args = actual.generic_args();
            for (i, a) in args.iter().enumerate() {
                if let Some(actual_arg_ty) = actual_args.get(i) {
                    if let Some(formal_inner) = a.as_type() {
                        unify_one(formal_inner, actual_arg_ty, subst);
                    }
                }
            }
            // Also try unifying against the actual's class name as
            // a whole — when the actual is just a bare class, the
            // formal class match itself is informationless.
            let _ = actual.base_class_name().map(|c| c == name.as_str());
        }
        JavaTypeSignature::Primitive(_) => {
            // Nothing to bind.
        }
    }
}

/// Top-level: given a method's signature and the call-site arg
/// types, build the substitution by unifying formals against args
/// pairwise. Receiver-as-first-arg is the caller's responsibility
/// (instance methods route the receiver to `args[0]`).
pub fn unify_call(signature: &MethodSignature, args: &[skotch_types::Ty]) -> Substitution {
    let mut subst = Substitution::new();
    for (formal, actual) in signature.param_tys.iter().zip(args.iter()) {
        unify_one(formal, actual, &mut subst);
    }
    subst
}

/// Convenience: infer the call's result type by unifying args and
/// substituting back into the return signature. When the signature
/// has no type parameters, this just returns the static return type.
pub fn infer_return_ty(signature: &MethodSignature, args: &[skotch_types::Ty]) -> skotch_types::Ty {
    let subst = unify_call(signature, args);
    signature_to_ty(&signature.return_ty, &subst)
}

/// Parse a method signature. Returns `None` for malformed input.
pub fn parse_method_signature(input: &str) -> Option<MethodSignature> {
    let mut p = Parser::new(input);
    let type_params = p.parse_type_params()?;
    p.expect('(')?;
    let mut param_tys = Vec::new();
    while p.peek() != Some(')') {
        param_tys.push(p.parse_type()?);
    }
    p.expect(')')?;
    let return_ty = p.parse_type()?;
    Some(MethodSignature {
        type_params,
        param_tys,
        return_ty,
    })
}

/// Parse a field signature. Returns `None` for malformed input.
pub fn parse_field_signature(input: &str) -> Option<JavaTypeSignature> {
    let mut p = Parser::new(input);
    let ty = p.parse_type()?;
    // Allow trailing chars (some signatures end with extra `;`).
    Some(ty)
}

// ── Internal ──────────────────────────────────────────────────────

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.bytes.get(self.pos).map(|&b| b as char)
    }

    fn expect(&mut self, c: char) -> Option<()> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    /// Parse the optional `<T:Bound;U:Bound2;>` header. Returns an
    /// empty vec when the input doesn't start with `<`.
    fn parse_type_params(&mut self) -> Option<Vec<TypeParam>> {
        if self.peek() != Some('<') {
            return Some(Vec::new());
        }
        self.pos += 1;
        let mut params = Vec::new();
        while self.peek() != Some('>') {
            // Identifier up to `:`.
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c == ':' || c == '>' {
                    break;
                }
                self.pos += 1;
            }
            let name = std::str::from_utf8(&self.bytes[start..self.pos])
                .ok()?
                .to_string();
            if name.is_empty() {
                return None;
            }
            let mut upper_bounds = Vec::new();
            // Class bound: `:LFoo;` (may be empty `::` if no class).
            self.expect(':')?;
            if self.peek() != Some(':') && self.peek() != Some('>') {
                upper_bounds.push(self.parse_type()?);
            }
            // Interface bounds: `:LIface1;:LIface2;`.
            while self.peek() == Some(':') {
                self.pos += 1;
                upper_bounds.push(self.parse_type()?);
            }
            params.push(TypeParam { name, upper_bounds });
        }
        self.expect('>')?;
        Some(params)
    }

    fn parse_type(&mut self) -> Option<JavaTypeSignature> {
        let c = self.peek()?;
        match c {
            'B' | 'S' | 'I' | 'J' | 'F' | 'D' | 'Z' | 'C' | 'V' => {
                self.pos += 1;
                Some(JavaTypeSignature::Primitive(c))
            }
            '[' => {
                self.pos += 1;
                let inner = self.parse_type()?;
                Some(JavaTypeSignature::Array(Box::new(inner)))
            }
            'L' => self.parse_class_type(),
            'T' => self.parse_type_var(),
            _ => None,
        }
    }

    fn parse_class_type(&mut self) -> Option<JavaTypeSignature> {
        self.expect('L')?;
        // Read class name up to `<`, `;`, or `.`.
        let mut name = String::new();
        let mut args = Vec::new();
        loop {
            let c = self.peek()?;
            match c {
                ';' => {
                    self.pos += 1;
                    return Some(JavaTypeSignature::ClassType { name, args });
                }
                '<' => {
                    self.pos += 1;
                    args = self.parse_type_args()?;
                    // After args, expect either ';' or '.' (inner class).
                }
                '.' => {
                    // Inner class: `Lpkg/Outer<X>.Inner<Y>;`. JVMS
                    // signatures express nested generics with this
                    // delimiter. We flatten to a single class name
                    // joined by `$` (JVM internal form for inner
                    // classes) and merge inner generic args onto the
                    // existing args list — coarse but adequate for
                    // descriptor lookups; the unifier doesn't need
                    // to distinguish inner-vs-outer arg slots.
                    self.pos += 1;
                    name.push('$');
                }
                _ => {
                    self.pos += 1;
                    name.push(c);
                }
            }
        }
    }

    fn parse_type_args(&mut self) -> Option<Vec<JavaTypeArg>> {
        let mut args = Vec::new();
        while self.peek() != Some('>') {
            match self.peek()? {
                '*' => {
                    self.pos += 1;
                    args.push(JavaTypeArg::Star);
                }
                '+' => {
                    self.pos += 1;
                    args.push(JavaTypeArg::ExtendsBound(self.parse_type()?));
                }
                '-' => {
                    self.pos += 1;
                    args.push(JavaTypeArg::SuperBound(self.parse_type()?));
                }
                _ => {
                    args.push(JavaTypeArg::Type(self.parse_type()?));
                }
            }
        }
        self.expect('>')?;
        Some(args)
    }

    fn parse_type_var(&mut self) -> Option<JavaTypeSignature> {
        self.expect('T')?;
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == ';' {
                break;
            }
            self.pos += 1;
        }
        let name = std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()?
            .to_string();
        self.expect(';')?;
        Some(JavaTypeSignature::TypeVar(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_listof_signature() {
        // `<T> fun listOf(vararg elements: T): List<T>`
        let sig = "<T:Ljava/lang/Object;>([TT;)Ljava/util/List<TT;>;";
        let m = parse_method_signature(sig).unwrap();
        assert_eq!(m.type_params.len(), 1);
        assert_eq!(m.type_params[0].name, "T");
        assert_eq!(m.param_tys.len(), 1);
        match &m.param_tys[0] {
            JavaTypeSignature::Array(inner) => match inner.as_ref() {
                JavaTypeSignature::TypeVar(n) => assert_eq!(n, "T"),
                _ => panic!("expected TypeVar inside array"),
            },
            _ => panic!("expected Array"),
        }
        match &m.return_ty {
            JavaTypeSignature::ClassType { name, args } => {
                assert_eq!(name, "java/util/List");
                assert_eq!(args.len(), 1);
                match &args[0] {
                    JavaTypeArg::Type(JavaTypeSignature::TypeVar(n)) => assert_eq!(n, "T"),
                    _ => panic!("expected TypeVar arg"),
                }
            }
            _ => panic!("expected ClassType return"),
        }
    }

    #[test]
    fn parse_let_signature() {
        // `<T, R> fun T.let(block: (T) -> R): R`
        // Compiled as a static helper: `<T,R>(T, Function1<-T, +R>) -> R`.
        let sig = "<T:Ljava/lang/Object;R:Ljava/lang/Object;>(\
                   TT;Lkotlin/jvm/functions/Function1<-TT;+TR;>;)TR;";
        let m = parse_method_signature(sig).unwrap();
        assert_eq!(m.type_params.len(), 2);
        assert_eq!(m.type_params[0].name, "T");
        assert_eq!(m.type_params[1].name, "R");
        assert_eq!(m.param_tys.len(), 2);
        match &m.return_ty {
            JavaTypeSignature::TypeVar(n) => assert_eq!(n, "R"),
            _ => panic!("expected TypeVar return"),
        }
    }

    #[test]
    fn parse_map_signature() {
        // `<T, R> fun Iterable<T>.map(transform: (T) -> R): List<R>`
        let sig = "<T:Ljava/lang/Object;R:Ljava/lang/Object;>(\
                   Ljava/lang/Iterable<TT;>;Lkotlin/jvm/functions/Function1<-TT;+TR;>;)\
                   Ljava/util/List<TR;>;";
        let m = parse_method_signature(sig).unwrap();
        assert_eq!(m.type_params.len(), 2);
        assert_eq!(m.param_tys.len(), 2);
        match &m.return_ty {
            JavaTypeSignature::ClassType { name, args } => {
                assert_eq!(name, "java/util/List");
                match &args[0] {
                    JavaTypeArg::Type(JavaTypeSignature::TypeVar(n)) => assert_eq!(n, "R"),
                    _ => panic!("expected TypeVar R"),
                }
            }
            _ => panic!("expected List<R>"),
        }
    }

    #[test]
    fn parse_field_signature_list_of_message() {
        let sig = "Ljava/util/List<Lcom/example/Message;>;";
        let ty = parse_field_signature(sig).unwrap();
        match ty {
            JavaTypeSignature::ClassType { name, args } => {
                assert_eq!(name, "java/util/List");
                assert_eq!(args.len(), 1);
                match &args[0] {
                    JavaTypeArg::Type(JavaTypeSignature::ClassType { name, .. }) => {
                        assert_eq!(name, "com/example/Message");
                    }
                    _ => panic!("unexpected arg"),
                }
            }
            _ => panic!("expected class type"),
        }
    }

    #[test]
    fn parse_wildcards() {
        let sig = "Ljava/util/List<*>;";
        match parse_field_signature(sig).unwrap() {
            JavaTypeSignature::ClassType { args, .. } => {
                assert!(matches!(args[0], JavaTypeArg::Star));
            }
            _ => panic!(),
        }
        let sig = "Ljava/util/List<+Lcom/example/Foo;>;";
        match parse_field_signature(sig).unwrap() {
            JavaTypeSignature::ClassType { args, .. } => match &args[0] {
                JavaTypeArg::ExtendsBound(JavaTypeSignature::ClassType { name, .. }) => {
                    assert_eq!(name, "com/example/Foo");
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn malformed_returns_none() {
        assert!(parse_method_signature("not a signature").is_none());
        assert!(parse_method_signature("()").is_none()); // missing return
    }

    // ── Unifier / substitution tests ─────────────────────────────

    #[test]
    fn signature_to_ty_listof_with_substituted_t() {
        use skotch_types::Ty;
        let sig =
            parse_method_signature("<T:Ljava/lang/Object;>([TT;)Ljava/util/List<TT;>;").unwrap();
        let mut subst = Substitution::new();
        subst.insert("T".to_string(), Ty::String);
        let result = signature_to_ty(&sig.return_ty, &subst);
        match result {
            Ty::Generic { base, args } => {
                assert_eq!(*base, Ty::Class("java/util/List".to_string()));
                assert_eq!(args, vec![Ty::String]);
            }
            other => panic!("expected List<String>, got {other:?}"),
        }
    }

    #[test]
    fn infer_listof_string_arg_returns_list_of_string() {
        use skotch_types::Ty;
        // Real listOf is `<T> ([T]) -> List<T>` but the array
        // formal doesn't contribute a binding through our unifier,
        // so we exercise the simpler `<T> (T) -> List<T>` shape
        // here.
        let sig =
            parse_method_signature("<T:Ljava/lang/Object;>(TT;)Ljava/util/List<TT;>;").unwrap();
        let result = infer_return_ty(&sig, &[Ty::String]);
        match result {
            Ty::Generic { args, .. } => assert_eq!(args, vec![Ty::String]),
            other => panic!("expected List<String>, got {other:?}"),
        }
    }

    #[test]
    fn infer_let_signature_from_function_arg() {
        use skotch_types::Ty;
        // `<T, R> (T, Function1<-T, +R>) -> R`
        let sig = parse_method_signature(
            "<T:Ljava/lang/Object;R:Ljava/lang/Object;>(TT;Lkotlin/jvm/functions/Function1<-TT;+TR;>;)TR;",
        )
        .unwrap();
        // Caller: `someUser.let { _ -> 42 }`
        // arg 0 is the receiver (User), arg 1 is a Function1
        // whose param is User and whose return is Int.
        let user = Ty::Class("com/example/User".to_string());
        let lambda = Ty::Function {
            params: vec![user.clone()],
            ret: Box::new(Ty::Int),
            is_suspend: false,
            is_composable: false,
        };
        let result = infer_return_ty(&sig, &[user.clone(), lambda]);
        assert_eq!(
            result,
            Ty::Int,
            "let's R should bind to Int from the lambda"
        );
    }

    #[test]
    fn infer_map_from_iterable_and_lambda() {
        use skotch_types::Ty;
        // `<T, R> (Iterable<T>, Function1<-T, +R>) -> List<R>`
        let sig = parse_method_signature(
            "<T:Ljava/lang/Object;R:Ljava/lang/Object;>(\
             Ljava/lang/Iterable<TT;>;Lkotlin/jvm/functions/Function1<-TT;+TR;>;)\
             Ljava/util/List<TR;>;",
        )
        .unwrap();
        // Caller: `users.map { it.name }` — receiver List<User>,
        // lambda (User) -> String.
        let user_class = Ty::Class("com/example/User".to_string());
        let users = Ty::Generic {
            base: Box::new(Ty::Class("java/util/List".to_string())),
            args: vec![user_class.clone()],
        };
        let lambda = Ty::Function {
            params: vec![user_class],
            ret: Box::new(Ty::String),
            is_suspend: false,
            is_composable: false,
        };
        let result = infer_return_ty(&sig, &[users, lambda]);
        match result {
            Ty::Generic { base, args } => {
                assert_eq!(*base, Ty::Class("java/util/List".to_string()));
                assert_eq!(args, vec![Ty::String], "map's R should bind to String");
            }
            other => panic!("expected List<String>, got {other:?}"),
        }
    }

    #[test]
    fn nullable_actual_unwraps_for_unification() {
        use skotch_types::Ty;
        let sig = parse_method_signature("<T:Ljava/lang/Object;>(TT;)TT;").unwrap();
        // Nullable<String> should unify T = String (variant info
        // is preserved on the actual but doesn't constrain T).
        let nullable_str = Ty::Nullable(Box::new(Ty::String));
        let result = infer_return_ty(&sig, &[nullable_str]);
        assert_eq!(result, Ty::String);
    }

    #[test]
    fn no_type_params_returns_static_return_ty() {
        use skotch_types::Ty;
        // Non-generic `String.length`-style: `()I`. The return is
        // just the literal `I` regardless of args.
        let sig = parse_method_signature("()I").unwrap();
        assert_eq!(infer_return_ty(&sig, &[]), Ty::Int);
    }
}
