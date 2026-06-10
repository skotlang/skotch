//! Typed wrapper definitions for the SIL composites.
//!
//! Every variant here matches one `SyntaxKind::*` composite — the
//! `cast()` method gates on that kind. Accessors expose the
//! load-bearing children (name idents, parameter lists, body blocks)
//! while preserving the underlying [`skotch_sil::SilNode`] for
//! callers that need to drop back to the untyped layer.
//!
//! The macros below cut boilerplate but keep the resulting types
//! discoverable in `cargo doc` — each typed wrapper still has its
//! own `pub struct`.

use crate::{children, first_typed_child, typed_children, AstNode, AstToken};
#[cfg(test)]
use crate::{children_of_kind, first_child_of_kind, first_typed_token};
use skotch_sil::{SilData, SilNode};
use skotch_syntax::SyntaxKind;

// ── macro: define a typed composite over one SyntaxKind ─────────────
macro_rules! ast_node {
    ($(#[$attr:meta])* $name:ident = $kind:ident) => {
        $(#[$attr])*
        #[derive(Copy, Clone, Debug)]
        pub struct $name<'a>(&'a SilNode);
        impl<'a> AstNode<'a> for $name<'a> {
            fn cast(node: &'a SilNode) -> Option<Self> {
                if node.kind == SyntaxKind::$kind {
                    Some(Self(node))
                } else {
                    None
                }
            }
            fn syntax(self) -> &'a SilNode {
                self.0
            }
        }
    };
}

// ── macro: define a typed token over one SyntaxKind ─────────────────
macro_rules! ast_token {
    ($(#[$attr:meta])* $name:ident = $kind:ident) => {
        $(#[$attr])*
        #[derive(Copy, Clone, Debug)]
        pub struct $name<'a>(&'a SilNode);
        impl<'a> AstToken<'a> for $name<'a> {
            fn cast(node: &'a SilNode) -> Option<Self> {
                if node.kind == SyntaxKind::$kind {
                    Some(Self(node))
                } else {
                    None
                }
            }
            fn syntax(self) -> &'a SilNode {
                self.0
            }
        }
    };
}

// ── File-level composites ───────────────────────────────────────────
ast_node!(
    /// The root `FILE` composite: package, imports, declarations.
    KtFile = FILE
);

ast_node!(
    /// `package x.y.z` directive (or empty when absent).
    KtPackageDirective = PACKAGE_DIRECTIVE
);

ast_node!(
    /// `IMPORT_LIST` wrapper around individual `IMPORT_DIRECTIVE`s.
    KtImportList = IMPORT_LIST
);

ast_node!(
    /// `import com.foo.Bar` or `import com.foo.* as Bar`.
    KtImportDirective = IMPORT_DIRECTIVE
);

ast_node!(
    /// `as Bar` rename clause on an import.
    KtImportAlias = IMPORT_ALIAS
);

ast_node!(KtFileAnnotationList = FILE_ANNOTATION_LIST);

// ── Declarations ────────────────────────────────────────────────────
//
// The SIL grammar emits a single `CLASS` composite for `class`,
// `interface`, and `enum class` declarations. The typed wrappers
// branch on the presence of `KW_INTERFACE` or `KW_ENUM` modifier
// inside the composite so consumers can pattern-match on declaration
// shape without re-checking keywords.

#[derive(Copy, Clone, Debug)]
pub struct KtClass<'a>(&'a SilNode);
impl<'a> AstNode<'a> for KtClass<'a> {
    fn cast(node: &'a SilNode) -> Option<Self> {
        if node.kind == SyntaxKind::CLASS && !class_is_interface(node) && !class_is_enum(node) {
            Some(Self(node))
        } else {
            None
        }
    }
    fn syntax(self) -> &'a SilNode {
        self.0
    }
}

#[derive(Copy, Clone, Debug)]
pub struct KtInterface<'a>(&'a SilNode);
impl<'a> AstNode<'a> for KtInterface<'a> {
    fn cast(node: &'a SilNode) -> Option<Self> {
        if node.kind == SyntaxKind::CLASS && class_is_interface(node) {
            Some(Self(node))
        } else {
            None
        }
    }
    fn syntax(self) -> &'a SilNode {
        self.0
    }
}

#[derive(Copy, Clone, Debug)]
pub struct KtEnumClass<'a>(&'a SilNode);
impl<'a> AstNode<'a> for KtEnumClass<'a> {
    fn cast(node: &'a SilNode) -> Option<Self> {
        if node.kind == SyntaxKind::CLASS && class_is_enum(node) {
            Some(Self(node))
        } else {
            None
        }
    }
    fn syntax(self) -> &'a SilNode {
        self.0
    }
}

fn class_is_interface(node: &SilNode) -> bool {
    children(node)
        .iter()
        .any(|c| c.kind == SyntaxKind::KW_INTERFACE)
}

fn class_is_enum(node: &SilNode) -> bool {
    // An `enum class` carries `KW_ENUM` in its MODIFIER_LIST.
    children(node).iter().any(|c| {
        if c.kind == SyntaxKind::MODIFIER_LIST {
            children(c).iter().any(|m| m.kind == SyntaxKind::KW_ENUM)
        } else {
            false
        }
    })
}

ast_node!(KtObjectDeclaration = OBJECT_DECLARATION);
ast_node!(KtCompanionObject = COMPANION_OBJECT);
ast_node!(KtEnumEntry = ENUM_ENTRY);
ast_node!(KtTypeAlias = TYPEALIAS);
ast_node!(KtFun = FUN);
ast_node!(KtProperty = PROPERTY);
ast_node!(KtPropertyAccessor = PROPERTY_ACCESSOR);
ast_node!(KtPrimaryConstructor = PRIMARY_CONSTRUCTOR);
ast_node!(KtSecondaryConstructor = SECONDARY_CONSTRUCTOR);
ast_node!(KtConstructorDelegationCall = CONSTRUCTOR_DELEGATION_CALL);
ast_node!(KtConstructorDelegationReference = CONSTRUCTOR_DELEGATION_REFERENCE);
ast_node!(KtClassBody = CLASS_BODY);
ast_node!(KtAnonymousInitializer = ANONYMOUS_INITIALIZER);

// ── Modifiers / annotations ────────────────────────────────────────
ast_node!(KtModifierList = MODIFIER_LIST);
ast_node!(KtAnnotation = ANNOTATION);
ast_node!(KtAnnotationEntry = ANNOTATION_ENTRY);
ast_node!(KtAnnotationUseSiteTarget = ANNOTATION_USE_SITE_TARGET);

// ── Parameters / arguments ─────────────────────────────────────────
ast_node!(KtValueParameterList = VALUE_PARAMETER_LIST);
ast_node!(KtValueParameter = VALUE_PARAMETER);
ast_node!(KtValueArgumentList = VALUE_ARGUMENT_LIST);
ast_node!(KtValueArgument = VALUE_ARGUMENT);
ast_node!(KtValueArgumentName = VALUE_ARGUMENT_NAME);
ast_node!(KtLambdaArgument = LAMBDA_ARGUMENT);

// ── Types ──────────────────────────────────────────────────────────
ast_node!(KtTypeParameterList = TYPE_PARAMETER_LIST);
ast_node!(KtTypeParameter = TYPE_PARAMETER);
ast_node!(KtTypeArgumentList = TYPE_ARGUMENT_LIST);
ast_node!(KtTypeProjection = TYPE_PROJECTION);
ast_node!(KtTypeReference = TYPE_REFERENCE);
ast_node!(KtUserType = USER_TYPE);
ast_node!(KtNullableType = NULLABLE_TYPE);
ast_node!(KtFunctionType = FUNCTION_TYPE);
ast_node!(KtFunctionTypeReceiver = FUNCTION_TYPE_RECEIVER);
ast_node!(KtDynamicType = DYNAMIC_TYPE);
ast_node!(KtTypeConstraintList = TYPE_CONSTRAINT_LIST);
ast_node!(KtTypeConstraint = TYPE_CONSTRAINT);

// ── Super types ────────────────────────────────────────────────────
ast_node!(KtSuperTypeList = SUPER_TYPE_LIST);
ast_node!(KtSuperTypeEntry = SUPER_TYPE_ENTRY);
ast_node!(KtDelegatedSuperTypeEntry = DELEGATED_SUPER_TYPE_ENTRY);
ast_node!(KtSuperTypeCallEntry = SUPER_TYPE_CALL_ENTRY);
ast_node!(KtConstructorCallee = CONSTRUCTOR_CALLEE);

// ── Statements / control flow ──────────────────────────────────────
ast_node!(KtBlock = BLOCK);
ast_node!(KtIf = IF);
ast_node!(KtThen = THEN);
ast_node!(KtElse = ELSE);
ast_node!(KtBody = BODY);
ast_node!(KtWhen = WHEN);
ast_node!(KtWhenEntry = WHEN_ENTRY);
ast_node!(KtWhenConditionInRange = WHEN_CONDITION_IN_RANGE);
ast_node!(KtWhenConditionIsPattern = WHEN_CONDITION_IS_PATTERN);
ast_node!(KtWhenConditionWithExpression = WHEN_CONDITION_WITH_EXPRESSION);
ast_node!(KtCondition = CONDITION);
ast_node!(KtFor = FOR);
ast_node!(KtWhile = WHILE);
ast_node!(KtDoWhile = DO_WHILE);
ast_node!(KtTry = TRY);
ast_node!(KtCatch = CATCH);
ast_node!(KtFinally = FINALLY);
ast_node!(KtReturn = RETURN);
ast_node!(KtThrow = THROW);
ast_node!(KtBreak = BREAK);
ast_node!(KtContinue = CONTINUE);
ast_node!(KtDestructuringDeclaration = DESTRUCTURING_DECLARATION);
ast_node!(KtDestructuringDeclarationEntry = DESTRUCTURING_DECLARATION_ENTRY);
ast_node!(KtLabeledStatement = LABELED_STATEMENT);
ast_node!(KtLabelQualifier = LABEL_QUALIFIER);
ast_node!(KtLabel = LABEL);
ast_node!(KtLoopRange = LOOP_RANGE);

// ── Expressions ────────────────────────────────────────────────────
ast_node!(KtBinaryExpression = BINARY_EXPRESSION);
ast_node!(KtBinaryWithTypeRhsExpression = BINARY_WITH_TYPE_RHS_EXPRESSION);
ast_node!(KtPrefixExpression = PREFIX_EXPRESSION);
ast_node!(KtPostfixExpression = POSTFIX_EXPRESSION);
ast_node!(KtUnaryExpression = UNARY_EXPRESSION);
ast_node!(KtOperationReference = OPERATION_REFERENCE);
ast_node!(KtDotQualifiedExpression = DOT_QUALIFIED_EXPRESSION);
ast_node!(KtSafeAccessExpression = SAFE_ACCESS_EXPRESSION);
ast_node!(KtReferenceExpression = REFERENCE_EXPRESSION);
ast_node!(KtThisExpression = THIS_EXPRESSION);
ast_node!(KtSuperExpression = SUPER_EXPRESSION);
ast_node!(KtCallExpression = CALL_EXPRESSION);
ast_node!(KtLambdaExpression = LAMBDA_EXPRESSION);
ast_node!(KtFunctionLiteral = FUNCTION_LITERAL);
ast_node!(KtArrayAccessExpression = ARRAY_ACCESS_EXPRESSION);
ast_node!(KtIndices = INDICES);
ast_node!(KtCallableReferenceExpression = CALLABLE_REFERENCE_EXPRESSION);
ast_node!(KtClassLiteralExpression = CLASS_LITERAL_EXPRESSION);
ast_node!(KtParenthesized = PARENTHESIZED);
ast_node!(KtCollectionLiteralExpression = COLLECTION_LITERAL_EXPRESSION);
ast_node!(KtAnnotatedExpression = ANNOTATED_EXPRESSION);
ast_node!(KtLabeledExpression = LABELED_EXPRESSION);
ast_node!(KtIsExpression = IS_EXPRESSION);
ast_node!(KtObjectLiteral = OBJECT_LITERAL);

// ── String templates ───────────────────────────────────────────────
ast_node!(KtStringTemplate = STRING_TEMPLATE);
ast_node!(KtLiteralStringTemplateEntry = LITERAL_STRING_TEMPLATE_ENTRY);
ast_node!(KtEscapeStringTemplateEntry = ESCAPE_STRING_TEMPLATE_ENTRY);
ast_node!(KtShortStringTemplateEntry = SHORT_STRING_TEMPLATE_ENTRY);
ast_node!(KtLongStringTemplateEntry = LONG_STRING_TEMPLATE_ENTRY);
ast_node!(KtBlockStringTemplateEntry = BLOCK_STRING_TEMPLATE_ENTRY);

// ── Constants ──────────────────────────────────────────────────────
ast_node!(KtIntegerConstant = INTEGER_CONSTANT);
ast_node!(KtFloatConstant = FLOAT_CONSTANT);
ast_node!(KtBooleanConstant = BOOLEAN_CONSTANT);
ast_node!(KtCharacterConstant = CHARACTER_CONSTANT);
ast_node!(KtNullConstant = NULL_CONSTANT);

// ── Tokens — only the few we need for ident-name lookup ────────────
ast_token!(KtIdentifier = IDENTIFIER);

// ── Enums grouping common shapes ───────────────────────────────────

/// A top-level declaration in a `KtFile`. Each variant wraps a typed
/// node; use `cast()` on each to pattern-match.
#[derive(Copy, Clone, Debug)]
pub enum KtDecl<'a> {
    Class(KtClass<'a>),
    Interface(KtInterface<'a>),
    Object(KtObjectDeclaration<'a>),
    EnumClass(KtEnumClass<'a>),
    TypeAlias(KtTypeAlias<'a>),
    Fun(KtFun<'a>),
    Property(KtProperty<'a>),
}

impl<'a> KtDecl<'a> {
    pub fn cast(node: &'a SilNode) -> Option<Self> {
        match node.kind {
            SyntaxKind::CLASS => {
                // The SIL parser emits a single CLASS composite for
                // class / interface / enum class. Route to the
                // matching typed wrapper by inspecting modifier/keyword
                // children.
                if let Some(i) = KtInterface::cast(node) {
                    return Some(Self::Interface(i));
                }
                if let Some(e) = KtEnumClass::cast(node) {
                    return Some(Self::EnumClass(e));
                }
                Some(Self::Class(KtClass::cast(node)?))
            }
            SyntaxKind::OBJECT_DECLARATION => Some(Self::Object(KtObjectDeclaration::cast(node)?)),
            SyntaxKind::TYPEALIAS => Some(Self::TypeAlias(KtTypeAlias::cast(node)?)),
            SyntaxKind::FUN => Some(Self::Fun(KtFun::cast(node)?)),
            SyntaxKind::PROPERTY => Some(Self::Property(KtProperty::cast(node)?)),
            _ => None,
        }
    }

    pub fn syntax(self) -> &'a SilNode {
        match self {
            Self::Class(n) => n.syntax(),
            Self::Interface(n) => n.syntax(),
            Self::Object(n) => n.syntax(),
            Self::EnumClass(n) => n.syntax(),
            Self::TypeAlias(n) => n.syntax(),
            Self::Fun(n) => n.syntax(),
            Self::Property(n) => n.syntax(),
        }
    }
}

/// An expression node — covers every composite kind that can appear
/// where an expression is expected.
#[derive(Copy, Clone, Debug)]
pub enum KtExpr<'a> {
    Reference(KtReferenceExpression<'a>),
    Integer(KtIntegerConstant<'a>),
    Float(KtFloatConstant<'a>),
    Boolean(KtBooleanConstant<'a>),
    Character(KtCharacterConstant<'a>),
    Null(KtNullConstant<'a>),
    String(KtStringTemplate<'a>),
    Binary(KtBinaryExpression<'a>),
    BinaryWithTypeRhs(KtBinaryWithTypeRhsExpression<'a>),
    Prefix(KtPrefixExpression<'a>),
    Postfix(KtPostfixExpression<'a>),
    Unary(KtUnaryExpression<'a>),
    DotQualified(KtDotQualifiedExpression<'a>),
    SafeAccess(KtSafeAccessExpression<'a>),
    This(KtThisExpression<'a>),
    Super(KtSuperExpression<'a>),
    Call(KtCallExpression<'a>),
    Lambda(KtLambdaExpression<'a>),
    ArrayAccess(KtArrayAccessExpression<'a>),
    CallableRef(KtCallableReferenceExpression<'a>),
    ClassLiteral(KtClassLiteralExpression<'a>),
    Parenthesized(KtParenthesized<'a>),
    Collection(KtCollectionLiteralExpression<'a>),
    Annotated(KtAnnotatedExpression<'a>),
    Labeled(KtLabeledExpression<'a>),
    Is(KtIsExpression<'a>),
    ObjectLiteral(KtObjectLiteral<'a>),
    If(KtIf<'a>),
    When(KtWhen<'a>),
    For(KtFor<'a>),
    While(KtWhile<'a>),
    DoWhile(KtDoWhile<'a>),
    Try(KtTry<'a>),
    Return(KtReturn<'a>),
    Throw(KtThrow<'a>),
    Break(KtBreak<'a>),
    Continue(KtContinue<'a>),
    Block(KtBlock<'a>),
}

impl<'a> KtExpr<'a> {
    pub fn cast(node: &'a SilNode) -> Option<Self> {
        use SyntaxKind as S;
        match node.kind {
            S::REFERENCE_EXPRESSION => KtReferenceExpression::cast(node).map(Self::Reference),
            S::INTEGER_CONSTANT => KtIntegerConstant::cast(node).map(Self::Integer),
            S::FLOAT_CONSTANT => KtFloatConstant::cast(node).map(Self::Float),
            S::BOOLEAN_CONSTANT => KtBooleanConstant::cast(node).map(Self::Boolean),
            S::CHARACTER_CONSTANT => KtCharacterConstant::cast(node).map(Self::Character),
            S::NULL_CONSTANT => KtNullConstant::cast(node).map(Self::Null),
            S::STRING_TEMPLATE => KtStringTemplate::cast(node).map(Self::String),
            S::BINARY_EXPRESSION => KtBinaryExpression::cast(node).map(Self::Binary),
            S::BINARY_WITH_TYPE_RHS_EXPRESSION => {
                KtBinaryWithTypeRhsExpression::cast(node).map(Self::BinaryWithTypeRhs)
            }
            S::PREFIX_EXPRESSION => KtPrefixExpression::cast(node).map(Self::Prefix),
            S::POSTFIX_EXPRESSION => KtPostfixExpression::cast(node).map(Self::Postfix),
            S::UNARY_EXPRESSION => KtUnaryExpression::cast(node).map(Self::Unary),
            S::DOT_QUALIFIED_EXPRESSION => {
                KtDotQualifiedExpression::cast(node).map(Self::DotQualified)
            }
            S::SAFE_ACCESS_EXPRESSION => KtSafeAccessExpression::cast(node).map(Self::SafeAccess),
            S::THIS_EXPRESSION => KtThisExpression::cast(node).map(Self::This),
            S::SUPER_EXPRESSION => KtSuperExpression::cast(node).map(Self::Super),
            S::CALL_EXPRESSION => KtCallExpression::cast(node).map(Self::Call),
            S::LAMBDA_EXPRESSION => KtLambdaExpression::cast(node).map(Self::Lambda),
            S::ARRAY_ACCESS_EXPRESSION => {
                KtArrayAccessExpression::cast(node).map(Self::ArrayAccess)
            }
            S::CALLABLE_REFERENCE_EXPRESSION => {
                KtCallableReferenceExpression::cast(node).map(Self::CallableRef)
            }
            S::CLASS_LITERAL_EXPRESSION => {
                KtClassLiteralExpression::cast(node).map(Self::ClassLiteral)
            }
            S::PARENTHESIZED => KtParenthesized::cast(node).map(Self::Parenthesized),
            S::COLLECTION_LITERAL_EXPRESSION => {
                KtCollectionLiteralExpression::cast(node).map(Self::Collection)
            }
            S::ANNOTATED_EXPRESSION => KtAnnotatedExpression::cast(node).map(Self::Annotated),
            S::LABELED_EXPRESSION => KtLabeledExpression::cast(node).map(Self::Labeled),
            S::IS_EXPRESSION => KtIsExpression::cast(node).map(Self::Is),
            S::OBJECT_LITERAL => KtObjectLiteral::cast(node).map(Self::ObjectLiteral),
            S::IF => KtIf::cast(node).map(Self::If),
            S::WHEN => KtWhen::cast(node).map(Self::When),
            S::FOR => KtFor::cast(node).map(Self::For),
            S::WHILE => KtWhile::cast(node).map(Self::While),
            S::DO_WHILE => KtDoWhile::cast(node).map(Self::DoWhile),
            S::TRY => KtTry::cast(node).map(Self::Try),
            S::RETURN => KtReturn::cast(node).map(Self::Return),
            S::THROW => KtThrow::cast(node).map(Self::Throw),
            S::BREAK => KtBreak::cast(node).map(Self::Break),
            S::CONTINUE => KtContinue::cast(node).map(Self::Continue),
            S::BLOCK => KtBlock::cast(node).map(Self::Block),
            _ => None,
        }
    }

    pub fn syntax(self) -> &'a SilNode {
        match self {
            Self::Reference(n) => n.syntax(),
            Self::Integer(n) => n.syntax(),
            Self::Float(n) => n.syntax(),
            Self::Boolean(n) => n.syntax(),
            Self::Character(n) => n.syntax(),
            Self::Null(n) => n.syntax(),
            Self::String(n) => n.syntax(),
            Self::Binary(n) => n.syntax(),
            Self::BinaryWithTypeRhs(n) => n.syntax(),
            Self::Prefix(n) => n.syntax(),
            Self::Postfix(n) => n.syntax(),
            Self::Unary(n) => n.syntax(),
            Self::DotQualified(n) => n.syntax(),
            Self::SafeAccess(n) => n.syntax(),
            Self::This(n) => n.syntax(),
            Self::Super(n) => n.syntax(),
            Self::Call(n) => n.syntax(),
            Self::Lambda(n) => n.syntax(),
            Self::ArrayAccess(n) => n.syntax(),
            Self::CallableRef(n) => n.syntax(),
            Self::ClassLiteral(n) => n.syntax(),
            Self::Parenthesized(n) => n.syntax(),
            Self::Collection(n) => n.syntax(),
            Self::Annotated(n) => n.syntax(),
            Self::Labeled(n) => n.syntax(),
            Self::Is(n) => n.syntax(),
            Self::ObjectLiteral(n) => n.syntax(),
            Self::If(n) => n.syntax(),
            Self::When(n) => n.syntax(),
            Self::For(n) => n.syntax(),
            Self::While(n) => n.syntax(),
            Self::DoWhile(n) => n.syntax(),
            Self::Try(n) => n.syntax(),
            Self::Return(n) => n.syntax(),
            Self::Throw(n) => n.syntax(),
            Self::Break(n) => n.syntax(),
            Self::Continue(n) => n.syntax(),
            Self::Block(n) => n.syntax(),
        }
    }
}

// ── Semantic accessors on the most common composites ──────────────

impl<'a> KtFile<'a> {
    /// File-level annotation list (`@file:JvmName(...)`).
    pub fn file_annotation_list(self) -> Option<KtFileAnnotationList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn package_directive(self) -> Option<KtPackageDirective<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn import_list(self) -> Option<KtImportList<'a>> {
        first_typed_child(self.syntax())
    }

    /// Iterate every top-level declaration in source order.
    pub fn decls(self) -> impl Iterator<Item = KtDecl<'a>> + 'a {
        children(self.syntax()).iter().filter_map(KtDecl::cast)
    }
}

impl<'a> KtPackageDirective<'a> {
    /// The dotted name as a contiguous string (no whitespace). The SIL
    /// shape nests qualified names inside `DOT_QUALIFIED_EXPRESSION`
    /// composites whose leaves are `REFERENCE_EXPRESSION { IDENTIFIER }`
    /// tokens.
    pub fn name(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        collect_package_idents(self.syntax(), &mut parts);
        parts.join(".")
    }

    pub fn is_empty(self) -> bool {
        children(self.syntax()).is_empty()
    }
}

fn collect_package_idents<'a>(node: &'a SilNode, out: &mut Vec<&'a str>) {
    for c in children(node) {
        match c.kind {
            SyntaxKind::IDENTIFIER => {
                if let SilData::Token { text } = &c.data {
                    out.push(text.as_str());
                }
            }
            SyntaxKind::REFERENCE_EXPRESSION | SyntaxKind::DOT_QUALIFIED_EXPRESSION => {
                collect_package_idents(c, out);
            }
            _ => {}
        }
    }
}

impl<'a> KtImportDirective<'a> {
    pub fn name(self) -> String {
        let mut s = String::new();
        for c in children(self.syntax()) {
            if matches!(c.kind, SyntaxKind::IDENTIFIER | SyntaxKind::DOT | SyntaxKind::MUL) {
                if let SilData::Token { text } = &c.data {
                    s.push_str(text);
                }
            }
            if c.kind == SyntaxKind::IMPORT_ALIAS {
                break;
            }
        }
        s
    }

    pub fn is_wildcard(self) -> bool {
        children(self.syntax())
            .iter()
            .any(|c| c.kind == SyntaxKind::MUL)
    }

    pub fn alias(self) -> Option<KtImportAlias<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtImportAlias<'a> {
    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().rev().find_map(|c| {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
            None
        })
    }
}

impl<'a> KtClass<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().find_map(|c| {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
            None
        })
    }

    pub fn type_parameter_list(self) -> Option<KtTypeParameterList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn primary_constructor(self) -> Option<KtPrimaryConstructor<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn super_type_list(self) -> Option<KtSuperTypeList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn body(self) -> Option<KtClassBody<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtClassBody<'a> {
    pub fn declarations(self) -> impl Iterator<Item = KtDecl<'a>> + 'a {
        children(self.syntax()).iter().filter_map(KtDecl::cast)
    }

    pub fn enum_entries(self) -> impl Iterator<Item = KtEnumEntry<'a>> + 'a {
        typed_children(self.syntax())
    }

    pub fn anonymous_initializers(self) -> impl Iterator<Item = KtAnonymousInitializer<'a>> + 'a {
        typed_children(self.syntax())
    }

    pub fn secondary_constructors(self) -> impl Iterator<Item = KtSecondaryConstructor<'a>> + 'a {
        typed_children(self.syntax())
    }
}

impl<'a> KtFun<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn name(self) -> Option<&'a str> {
        let mut after_fun = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::KW_FUN {
                after_fun = true;
                continue;
            }
            if after_fun && c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
        }
        None
    }

    pub fn type_parameter_list(self) -> Option<KtTypeParameterList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn value_parameter_list(self) -> Option<KtValueParameterList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn return_type(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn body_block(self) -> Option<KtBlock<'a>> {
        first_typed_child(self.syntax())
    }

    /// `= expr` body (when the function uses expression body).
    pub fn body_expression(self) -> Option<KtExpr<'a>> {
        let mut after_eq = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::EQ {
                after_eq = true;
                continue;
            }
            if after_eq {
                if let Some(e) = KtExpr::cast(c) {
                    return Some(e);
                }
            }
        }
        None
    }
}

impl<'a> KtProperty<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn is_var(self) -> bool {
        children(self.syntax())
            .iter()
            .any(|c| c.kind == SyntaxKind::KW_VAR)
    }

    pub fn name(self) -> Option<&'a str> {
        let mut after_kw = false;
        for c in children(self.syntax()) {
            if matches!(c.kind, SyntaxKind::KW_VAL | SyntaxKind::KW_VAR) {
                after_kw = true;
                continue;
            }
            if after_kw && c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
        }
        None
    }

    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn initializer(self) -> Option<KtExpr<'a>> {
        let mut after_eq = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::EQ {
                after_eq = true;
                continue;
            }
            if after_eq {
                if let Some(e) = KtExpr::cast(c) {
                    return Some(e);
                }
            }
        }
        None
    }

    pub fn destructuring(self) -> Option<KtDestructuringDeclaration<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtValueParameter<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().find_map(|c| {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
            None
        })
    }

    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn default_value(self) -> Option<KtExpr<'a>> {
        let mut after_eq = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::EQ {
                after_eq = true;
                continue;
            }
            if after_eq {
                if let Some(e) = KtExpr::cast(c) {
                    return Some(e);
                }
            }
        }
        None
    }
}

impl<'a> KtBlock<'a> {
    pub fn statements(self) -> impl Iterator<Item = KtExpr<'a>> + 'a {
        children(self.syntax()).iter().filter_map(KtExpr::cast)
    }
}

impl<'a> KtReferenceExpression<'a> {
    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().find_map(|c| {
            if let SilData::Token { text } = &c.data {
                Some(text.as_str())
            } else {
                None
            }
        })
    }
}

impl<'a> KtCallExpression<'a> {
    pub fn callee(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }

    pub fn value_argument_list(self) -> Option<KtValueArgumentList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn lambda_argument(self) -> Option<KtLambdaArgument<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn type_argument_list(self) -> Option<KtTypeArgumentList<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtValueArgumentList<'a> {
    pub fn arguments(self) -> impl Iterator<Item = KtValueArgument<'a>> + 'a {
        typed_children(self.syntax())
    }
}

impl<'a> KtValueArgument<'a> {
    pub fn name(self) -> Option<&'a str> {
        let name_node = first_typed_child::<KtValueArgumentName>(self.syntax())?;
        name_node
            .syntax()
            .data
            .as_composite()
            .and_then(|children| {
                children.iter().find_map(|c| {
                    if c.kind == SyntaxKind::REFERENCE_EXPRESSION {
                        KtReferenceExpression::cast(c)?.name()
                    } else {
                        None
                    }
                })
            })
    }

    pub fn expression(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }
}

impl<'a> KtIf<'a> {
    pub fn condition(self) -> Option<KtCondition<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn then_branch(self) -> Option<KtThen<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn else_branch(self) -> Option<KtElse<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtCondition<'a> {
    pub fn expression(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }
}

impl<'a> KtThen<'a> {
    pub fn expression(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }
}

impl<'a> KtElse<'a> {
    pub fn expression(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }
}

impl<'a> KtBinaryExpression<'a> {
    pub fn lhs(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }

    pub fn rhs(self) -> Option<KtExpr<'a>> {
        children(self.syntax())
            .iter()
            .filter_map(KtExpr::cast)
            .nth(1)
    }

    pub fn operation(self) -> Option<KtOperationReference<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtOperationReference<'a> {
    pub fn text(self) -> String {
        let mut s = String::new();
        for c in children(self.syntax()) {
            if let SilData::Token { text } = &c.data {
                s.push_str(text);
            }
        }
        s
    }
}

impl<'a> KtModifierList<'a> {
    /// Iterate every modifier-keyword child (e.g. `KW_OPEN`,
    /// `KW_OVERRIDE`). Annotations are excluded.
    pub fn modifier_kinds(self) -> impl Iterator<Item = SyntaxKind> + 'a {
        children(self.syntax()).iter().filter_map(|c| match c.kind {
            SyntaxKind::ANNOTATION_ENTRY | SyntaxKind::ANNOTATION => None,
            k if k.is_token() => Some(k),
            _ => None,
        })
    }

    pub fn annotations(self) -> impl Iterator<Item = KtAnnotationEntry<'a>> + 'a {
        typed_children(self.syntax())
    }

    pub fn has_kind(self, kind: SyntaxKind) -> bool {
        self.modifier_kinds().any(|k| k == kind)
    }

    /// Resolve the visibility encoded in this modifier list. Defaults
    /// to `Public` when no visibility modifier is present.
    pub fn visibility(self) -> skotch_syntax::Visibility {
        use skotch_syntax::Visibility as V;
        if self.has_kind(SyntaxKind::KW_PRIVATE) {
            V::Private
        } else if self.has_kind(SyntaxKind::KW_PROTECTED) {
            V::Protected
        } else if self.has_kind(SyntaxKind::KW_INTERNAL) {
            V::Internal
        } else {
            V::Public
        }
    }

    /// True when the modifier list has an `@Foo` annotation entry whose
    /// short name matches `name` (case-sensitive, FQ prefix stripped).
    pub fn has_annotation(self, name: &str) -> bool {
        self.annotations().any(|a| {
            a.short_name()
                .map(|n| n == name)
                .unwrap_or(false)
        })
    }
}

// ── KtAnnotationEntry ───────────────────────────────────────────────

impl<'a> KtAnnotationEntry<'a> {
    /// The constructor-callee identifier of the annotation. For a
    /// qualified annotation like `@kotlin.jvm.JvmStatic`, returns
    /// `"JvmStatic"` (the unqualified short tail of the dotted path).
    pub fn short_name(self) -> Option<&'a str> {
        // Walk the callee composite (CONSTRUCTOR_CALLEE) for the
        // last IDENTIFIER token.
        let callee = first_typed_child::<KtConstructorCallee>(self.syntax())?;
        let mut last: Option<&str> = None;
        for c in children(callee.syntax()) {
            // The callee body may have nested type refs / user types.
            // Walk one level deep to collect identifier tokens.
            collect_last_ident_token(c, &mut last);
        }
        last
    }

    /// The optional `@field:JvmName` use-site target token text
    /// (`field`, `param`, `get`, `set`, etc.).
    pub fn use_site_target(self) -> Option<&'a str> {
        let t = first_typed_child::<KtAnnotationUseSiteTarget>(self.syntax())?;
        children(t.syntax()).iter().find_map(|c| {
            if let SilData::Token { text } = &c.data {
                Some(text.as_str())
            } else {
                None
            }
        })
    }

    pub fn value_argument_list(self) -> Option<KtValueArgumentList<'a>> {
        first_typed_child(self.syntax())
    }
}

fn collect_last_ident_token<'a>(node: &'a SilNode, sink: &mut Option<&'a str>) {
    match &node.data {
        SilData::Token { text } if node.kind == SyntaxKind::IDENTIFIER => {
            *sink = Some(text.as_str());
        }
        SilData::Composite { children: cs } => {
            for c in cs {
                collect_last_ident_token(c, sink);
            }
        }
        _ => {}
    }
}

// ── KtClass: full modifier / visibility / supertype surface ─────────

impl<'a> KtClass<'a> {
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
    pub fn is_data(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_DATA))
            .unwrap_or(false)
    }
    pub fn is_open(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_OPEN))
            .unwrap_or(false)
    }
    pub fn is_abstract(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_ABSTRACT))
            .unwrap_or(false)
    }
    pub fn is_sealed(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_SEALED))
            .unwrap_or(false)
    }
    pub fn is_inner(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_INNER))
            .unwrap_or(false)
    }
    /// Annotation short-names on this class (`"Composable"`,
    /// `"Deprecated"`, …). Use-site target prefixes are stripped.
    pub fn annotation_names(self) -> Vec<&'a str> {
        self.modifier_list()
            .map(|m| m.annotations().filter_map(|a| a.short_name()).collect())
            .unwrap_or_default()
    }
}

// ── KtInterface accessors ───────────────────────────────────────────

impl<'a> KtInterface<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().find_map(|c| {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
            None
        })
    }
    pub fn type_parameter_list(self) -> Option<KtTypeParameterList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn super_type_list(self) -> Option<KtSuperTypeList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtClassBody<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
    pub fn is_sealed(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_SEALED))
            .unwrap_or(false)
    }
    pub fn is_fun_interface(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_FUN_INTERFACE))
            .unwrap_or(false)
    }
}

// ── KtObjectDeclaration accessors ───────────────────────────────────

impl<'a> KtObjectDeclaration<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn name(self) -> Option<&'a str> {
        let mut after_kw = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::KW_OBJECT {
                after_kw = true;
                continue;
            }
            if after_kw && c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
        }
        None
    }
    pub fn super_type_list(self) -> Option<KtSuperTypeList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtClassBody<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
    pub fn is_companion(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_COMPANION))
            .unwrap_or(false)
    }
}

// ── KtEnumClass accessors ───────────────────────────────────────────

impl<'a> KtEnumClass<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().find_map(|c| {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
            None
        })
    }
    pub fn primary_constructor(self) -> Option<KtPrimaryConstructor<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn super_type_list(self) -> Option<KtSuperTypeList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtClassBody<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
}

// ── KtTypeAlias accessors ───────────────────────────────────────────

impl<'a> KtTypeAlias<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn name(self) -> Option<&'a str> {
        let mut after_kw = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::KW_TYPEALIAS {
                after_kw = true;
                continue;
            }
            if after_kw && c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
        }
        None
    }
    pub fn type_parameter_list(self) -> Option<KtTypeParameterList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
}

// ── KtFun: full modifier / visibility / receiver surface ────────────

impl<'a> KtFun<'a> {
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
    pub fn is_open(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_OPEN))
            .unwrap_or(false)
    }
    pub fn is_override(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_OVERRIDE))
            .unwrap_or(false)
    }
    pub fn is_abstract(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_ABSTRACT))
            .unwrap_or(false)
    }
    pub fn is_suspend(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_SUSPEND))
            .unwrap_or(false)
    }
    pub fn is_inline(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_INLINE))
            .unwrap_or(false)
    }
    pub fn is_operator(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_OPERATOR))
            .unwrap_or(false)
    }
    pub fn is_infix(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_INFIX))
            .unwrap_or(false)
    }
    pub fn is_tailrec(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_TAILREC))
            .unwrap_or(false)
    }
    pub fn annotation_names(self) -> Vec<&'a str> {
        self.modifier_list()
            .map(|m| m.annotations().filter_map(|a| a.short_name()).collect())
            .unwrap_or_default()
    }
    /// Extension-fn receiver type. The receiver is a `TYPE_REFERENCE`
    /// appearing before the function name in source. We surface it as
    /// the first `KtTypeReference` whose position is BEFORE the
    /// function name token. (The return type's `KtTypeReference`
    /// appears AFTER `RPAR`.)
    pub fn receiver_type(self) -> Option<KtTypeReference<'a>> {
        let mut found_name = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::KW_FUN {
                continue;
            }
            if c.kind == SyntaxKind::IDENTIFIER && !found_name {
                // The fn-name identifier. If we haven't yet seen a
                // TYPE_REFERENCE child, there's no receiver.
                return None;
            }
            if c.kind == SyntaxKind::TYPE_REFERENCE && !found_name {
                return KtTypeReference::cast(c);
            }
            if c.kind == SyntaxKind::DOT && !found_name {
                // After a `Type .`, the next IDENTIFIER is the fn name.
                found_name = true;
            }
        }
        None
    }
}

// ── KtProperty: full surface ────────────────────────────────────────

impl<'a> KtProperty<'a> {
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
    pub fn is_lateinit(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_LATEINIT))
            .unwrap_or(false)
    }
    pub fn is_const(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_CONST))
            .unwrap_or(false)
    }
    pub fn annotation_names(self) -> Vec<&'a str> {
        self.modifier_list()
            .map(|m| m.annotations().filter_map(|a| a.short_name()).collect())
            .unwrap_or_default()
    }
    pub fn property_accessors(self) -> impl Iterator<Item = KtPropertyAccessor<'a>> + 'a {
        typed_children(self.syntax())
    }
    /// Receiver type for extension properties: `val String.lastChar`.
    /// Same shape as KtFun::receiver_type.
    pub fn receiver_type(self) -> Option<KtTypeReference<'a>> {
        for c in children(self.syntax()) {
            if matches!(c.kind, SyntaxKind::KW_VAL | SyntaxKind::KW_VAR) {
                continue;
            }
            if c.kind == SyntaxKind::TYPE_REFERENCE {
                // Could be the property type OR a receiver type.
                // It's a receiver iff followed by a DOT before the name.
                // Probe forward.
                let mut after = false;
                let mut saw_dot = false;
                for d in children(self.syntax()) {
                    if std::ptr::eq(d, c) {
                        after = true;
                        continue;
                    }
                    if after {
                        if d.kind == SyntaxKind::DOT {
                            saw_dot = true;
                            break;
                        }
                        if d.kind == SyntaxKind::IDENTIFIER {
                            break;
                        }
                    }
                }
                return if saw_dot { KtTypeReference::cast(c) } else { None };
            }
            if c.kind == SyntaxKind::IDENTIFIER {
                return None;
            }
        }
        None
    }
}

// ── KtValueParameter: full surface ──────────────────────────────────

impl<'a> KtValueParameter<'a> {
    pub fn is_vararg(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_VARARG))
            .unwrap_or(false)
    }
    pub fn is_val(self) -> bool {
        children(self.syntax())
            .iter()
            .any(|c| c.kind == SyntaxKind::KW_VAL)
    }
    pub fn is_var(self) -> bool {
        children(self.syntax())
            .iter()
            .any(|c| c.kind == SyntaxKind::KW_VAR)
    }
    pub fn is_crossinline(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_CROSSINLINE))
            .unwrap_or(false)
    }
    pub fn is_noinline(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_NOINLINE))
            .unwrap_or(false)
    }
    pub fn annotation_names(self) -> Vec<&'a str> {
        self.modifier_list()
            .map(|m| m.annotations().filter_map(|a| a.short_name()).collect())
            .unwrap_or_default()
    }
}

// ── KtPrimaryConstructor / KtSecondaryConstructor ───────────────────

impl<'a> KtPrimaryConstructor<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn value_parameter_list(self) -> Option<KtValueParameterList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
}

impl<'a> KtSecondaryConstructor<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn value_parameter_list(self) -> Option<KtValueParameterList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtBlock<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn delegation_call(self) -> Option<KtConstructorDelegationCall<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn visibility(self) -> skotch_syntax::Visibility {
        self.modifier_list()
            .map(|m| m.visibility())
            .unwrap_or(skotch_syntax::Visibility::Public)
    }
}

impl<'a> KtConstructorDelegationCall<'a> {
    pub fn value_argument_list(self) -> Option<KtValueArgumentList<'a>> {
        first_typed_child(self.syntax())
    }
    /// True when the delegation is `super(...)` rather than `this(...)`.
    pub fn is_super(self) -> bool {
        children(self.syntax()).iter().any(|c| {
            matches!(c.kind, SyntaxKind::KW_SUPER)
                || c.kind == SyntaxKind::CONSTRUCTOR_DELEGATION_REFERENCE
                    && children(c).iter().any(|cc| cc.kind == SyntaxKind::KW_SUPER)
        })
    }
}

impl<'a> KtValueParameterList<'a> {
    pub fn parameters(self) -> impl Iterator<Item = KtValueParameter<'a>> + 'a {
        typed_children(self.syntax())
    }
}

// ── Type-reference family ───────────────────────────────────────────

impl<'a> KtTypeReference<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn is_suspend(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_SUSPEND))
            .unwrap_or(false)
    }

    pub fn is_composable(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_annotation("Composable"))
            .unwrap_or(false)
    }

    /// Distinguish among the three kinds of typed children: user type,
    /// function type, nullable wrapper, dynamic.
    pub fn user_type(self) -> Option<KtUserType<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn function_type(self) -> Option<KtFunctionType<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn nullable_type(self) -> Option<KtNullableType<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn dynamic_type(self) -> Option<KtDynamicType<'a>> {
        first_typed_child(self.syntax())
    }

    pub fn is_nullable(self) -> bool {
        self.nullable_type().is_some()
    }
}

impl<'a> KtNullableType<'a> {
    /// The unwrapped inner type — directly a `KtUserType` or
    /// `KtFunctionType`, NOT wrapped in a `TYPE_REFERENCE` in the SIL
    /// shape (the `?` marker lives on the NULLABLE_TYPE composite, not
    /// at the TYPE_REFERENCE level).
    pub fn inner_user_type(self) -> Option<KtUserType<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn inner_function_type(self) -> Option<KtFunctionType<'a>> {
        first_typed_child(self.syntax())
    }
    /// Recursively wrapped NULLABLE_TYPE (rare).
    pub fn inner_nullable(self) -> Option<KtNullableType<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtUserType<'a> {
    /// The optional qualifier — a preceding USER_TYPE composite for
    /// dotted names like `kotlin.collections.List`.
    pub fn qualifier(self) -> Option<KtUserType<'a>> {
        first_typed_child(self.syntax())
    }

    /// The bare (unqualified) tail name of this user type. The SIL
    /// shape stores it as `REFERENCE_EXPRESSION { IDENTIFIER }` —
    /// we dig through the immediate REFERENCE_EXPRESSION child.
    pub fn name(self) -> Option<&'a str> {
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::REFERENCE_EXPRESSION {
                if let Some(r) = KtReferenceExpression::cast(c) {
                    if let Some(n) = r.name() {
                        return Some(n);
                    }
                }
            }
        }
        None
    }

    pub fn type_argument_list(self) -> Option<KtTypeArgumentList<'a>> {
        first_typed_child(self.syntax())
    }

    /// Dotted FQ name: `kotlin.collections.List` → `kotlin.collections.List`.
    pub fn dotted_name(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        fn rec<'a>(u: KtUserType<'a>, out: &mut Vec<&'a str>) {
            if let Some(q) = u.qualifier() {
                rec(q, out);
            }
            if let Some(n) = u.name() {
                out.push(n);
            }
        }
        rec(self, &mut parts);
        parts.join(".")
    }
}

impl<'a> KtFunctionType<'a> {
    pub fn receiver(self) -> Option<KtFunctionTypeReceiver<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn parameter_list(self) -> Option<KtValueParameterList<'a>> {
        first_typed_child(self.syntax())
    }
    /// Return type — the TYPE_REFERENCE appearing AFTER the `->` arrow.
    pub fn return_type(self) -> Option<KtTypeReference<'a>> {
        let mut after_arrow = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::ARROW {
                after_arrow = true;
                continue;
            }
            if after_arrow && c.kind == SyntaxKind::TYPE_REFERENCE {
                return KtTypeReference::cast(c);
            }
        }
        None
    }
}

impl<'a> KtFunctionTypeReceiver<'a> {
    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtTypeArgumentList<'a> {
    pub fn arguments(self) -> impl Iterator<Item = KtTypeProjection<'a>> + 'a {
        typed_children(self.syntax())
    }
}

impl<'a> KtTypeProjection<'a> {
    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }
    /// `out T`, `in T`, or invariant (None).
    pub fn variance(self) -> Option<SyntaxKind> {
        children(self.syntax()).iter().find_map(|c| {
            if matches!(c.kind, SyntaxKind::KW_OUT | SyntaxKind::KW_IN) {
                Some(c.kind)
            } else {
                None
            }
        })
    }
}

impl<'a> KtTypeParameterList<'a> {
    pub fn parameters(self) -> impl Iterator<Item = KtTypeParameter<'a>> + 'a {
        typed_children(self.syntax())
    }
}

impl<'a> KtTypeParameter<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().find_map(|c| {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
            None
        })
    }
    pub fn upper_bound(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn is_reified(self) -> bool {
        self.modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_REIFIED))
            .unwrap_or(false)
    }
}

// ── Super-type list / entries ───────────────────────────────────────

impl<'a> KtSuperTypeList<'a> {
    pub fn entries(self) -> impl Iterator<Item = SuperTypeEntry<'a>> + 'a {
        children(self.syntax()).iter().filter_map(SuperTypeEntry::cast)
    }
}

/// Union of the three super-type-entry kinds.
#[derive(Copy, Clone, Debug)]
pub enum SuperTypeEntry<'a> {
    /// Plain `SuperClass` clause.
    Type(KtSuperTypeEntry<'a>),
    /// `SuperClass(args)` constructor call.
    Call(KtSuperTypeCallEntry<'a>),
    /// `Interface by delegateExpr`.
    Delegated(KtDelegatedSuperTypeEntry<'a>),
}

impl<'a> SuperTypeEntry<'a> {
    pub fn cast(node: &'a SilNode) -> Option<Self> {
        match node.kind {
            SyntaxKind::SUPER_TYPE_ENTRY => KtSuperTypeEntry::cast(node).map(Self::Type),
            SyntaxKind::SUPER_TYPE_CALL_ENTRY => KtSuperTypeCallEntry::cast(node).map(Self::Call),
            SyntaxKind::DELEGATED_SUPER_TYPE_ENTRY => {
                KtDelegatedSuperTypeEntry::cast(node).map(Self::Delegated)
            }
            _ => None,
        }
    }
    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        match self {
            Self::Type(e) => e.type_reference(),
            Self::Call(e) => e.callee_type(),
            Self::Delegated(e) => e.type_reference(),
        }
    }
}

impl<'a> KtSuperTypeEntry<'a> {
    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtSuperTypeCallEntry<'a> {
    /// The bare class type being invoked (`Base` in `Base(arg)`).
    pub fn callee_type(self) -> Option<KtTypeReference<'a>> {
        first_typed_child::<KtConstructorCallee>(self.syntax())
            .and_then(|c| first_typed_child::<KtTypeReference>(c.syntax()))
    }
    pub fn value_argument_list(self) -> Option<KtValueArgumentList<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtDelegatedSuperTypeEntry<'a> {
    pub fn type_reference(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn delegate_expression(self) -> Option<KtExpr<'a>> {
        // Expression appearing AFTER the `by` keyword.
        let mut after_by = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::KW_BY {
                after_by = true;
                continue;
            }
            if after_by {
                if let Some(e) = KtExpr::cast(c) {
                    return Some(e);
                }
            }
        }
        None
    }
}

// ── KtEnumEntry ──────────────────────────────────────────────────────

impl<'a> KtEnumEntry<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn name(self) -> Option<&'a str> {
        children(self.syntax()).iter().find_map(|c| {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    return Some(text.as_str());
                }
            }
            None
        })
    }
    pub fn value_argument_list(self) -> Option<KtValueArgumentList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtClassBody<'a>> {
        first_typed_child(self.syntax())
    }
}

// ── KtPropertyAccessor ──────────────────────────────────────────────

impl<'a> KtPropertyAccessor<'a> {
    pub fn modifier_list(self) -> Option<KtModifierList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn is_getter(self) -> bool {
        children(self.syntax())
            .iter()
            .any(|c| c.kind == SyntaxKind::KW_GET)
    }
    pub fn is_setter(self) -> bool {
        children(self.syntax())
            .iter()
            .any(|c| c.kind == SyntaxKind::KW_SET)
    }
    pub fn value_parameter_list(self) -> Option<KtValueParameterList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body_block(self) -> Option<KtBlock<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body_expression(self) -> Option<KtExpr<'a>> {
        let mut after_eq = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::EQ {
                after_eq = true;
                continue;
            }
            if after_eq {
                if let Some(e) = KtExpr::cast(c) {
                    return Some(e);
                }
            }
        }
        None
    }
    pub fn return_type(self) -> Option<KtTypeReference<'a>> {
        first_typed_child(self.syntax())
    }
}

// ── Loop / when / try shapes ────────────────────────────────────────

impl<'a> KtWhile<'a> {
    pub fn condition(self) -> Option<KtCondition<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtBody<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtDoWhile<'a> {
    pub fn condition(self) -> Option<KtCondition<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtBody<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtFor<'a> {
    /// The loop parameter (`for (i in ...) { ... }` → `i`).
    pub fn loop_parameter(self) -> Option<KtValueParameter<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn loop_range(self) -> Option<KtLoopRange<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtBody<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn destructuring(self) -> Option<KtDestructuringDeclaration<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtBody<'a> {
    pub fn expression(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }
}

impl<'a> KtLoopRange<'a> {
    pub fn expression(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }
}

impl<'a> KtWhen<'a> {
    /// Optional subject (`when (x)` → expression; `when {}` → None).
    pub fn subject(self) -> Option<KtExpr<'a>> {
        children(self.syntax()).iter().find_map(KtExpr::cast)
    }
    pub fn entries(self) -> impl Iterator<Item = KtWhenEntry<'a>> + 'a {
        typed_children(self.syntax())
    }
}

impl<'a> KtWhenEntry<'a> {
    /// Each branch may have multiple conditions (`1, 2 -> ...`).
    pub fn conditions(self) -> Vec<&'a SilNode> {
        children(self.syntax())
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    SyntaxKind::WHEN_CONDITION_IN_RANGE
                        | SyntaxKind::WHEN_CONDITION_IS_PATTERN
                        | SyntaxKind::WHEN_CONDITION_WITH_EXPRESSION
                )
            })
            .collect()
    }
    pub fn body(self) -> Option<KtExpr<'a>> {
        // The body comes after the `->` arrow token.
        let mut after_arrow = false;
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::ARROW {
                after_arrow = true;
                continue;
            }
            if after_arrow {
                if let Some(e) = KtExpr::cast(c) {
                    return Some(e);
                }
            }
        }
        None
    }
    pub fn is_else(self) -> bool {
        children(self.syntax())
            .iter()
            .any(|c| c.kind == SyntaxKind::KW_ELSE)
    }
}

impl<'a> KtTry<'a> {
    pub fn try_block(self) -> Option<KtBlock<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn catches(self) -> impl Iterator<Item = KtCatch<'a>> + 'a {
        typed_children(self.syntax())
    }
    pub fn finally(self) -> Option<KtFinally<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtCatch<'a> {
    /// The `catch (e: Exception)` parameter — wrapped in a
    /// VALUE_PARAMETER_LIST in the SIL shape (matches Kotlin's
    /// concrete-syntax requirement of parens around the catch var).
    pub fn parameter(self) -> Option<KtValueParameter<'a>> {
        let plist = first_typed_child::<KtValueParameterList>(self.syntax())?;
        plist.parameters().next()
    }
    pub fn body(self) -> Option<KtBlock<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtFinally<'a> {
    pub fn body(self) -> Option<KtBlock<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtLambdaExpression<'a> {
    pub fn function_literal(self) -> Option<KtFunctionLiteral<'a>> {
        first_typed_child(self.syntax())
    }
}

impl<'a> KtFunctionLiteral<'a> {
    pub fn value_parameter_list(self) -> Option<KtValueParameterList<'a>> {
        first_typed_child(self.syntax())
    }
    pub fn body(self) -> Option<KtBlock<'a>> {
        first_typed_child(self.syntax())
    }
}

// ── KtImportDirective helpers ───────────────────────────────────────

impl<'a> KtImportDirective<'a> {
    /// Split the dotted import path: `import com.foo.Bar` → ["com",
    /// "foo", "Bar"].
    pub fn name_parts(self) -> Vec<&'a str> {
        let mut parts = Vec::new();
        for c in children(self.syntax()) {
            if c.kind == SyntaxKind::IDENTIFIER {
                if let SilData::Token { text } = &c.data {
                    parts.push(text.as_str());
                }
            }
            if c.kind == SyntaxKind::IMPORT_ALIAS {
                break;
            }
        }
        parts
    }
}

// SilData helper used by ValueArgument above.
trait SilDataExt {
    fn as_composite(&self) -> Option<&[SilNode]>;
}

impl SilDataExt for SilData {
    fn as_composite(&self) -> Option<&[SilNode]> {
        match self {
            SilData::Composite { children } => Some(children),
            _ => None,
        }
    }
}

// Suppress unused-import warning when nothing in this file uses these.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_file() {
        let parsed = crate::parse("test.kt", "fun main() {}");
        let file = parsed.file();
        let decls: Vec<_> = file.decls().collect();
        assert_eq!(decls.len(), 1);
        match decls[0] {
            KtDecl::Fun(f) => assert_eq!(f.name(), Some("main")),
            _ => panic!("expected fun"),
        }
    }

    #[test]
    fn parse_class_with_property() {
        let src = "class Holder {\n    val x: Int = 7\n}";
        let parsed = crate::parse("test.kt", src);
        let file = parsed.file();
        let decls: Vec<_> = file.decls().collect();
        assert_eq!(decls.len(), 1);
        let cls = match decls[0] {
            KtDecl::Class(c) => c,
            _ => panic!("expected class"),
        };
        assert_eq!(cls.name(), Some("Holder"));
        let body = cls.body().expect("class body");
        let props: Vec<_> = body
            .declarations()
            .filter_map(|d| match d {
                KtDecl::Property(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].name(), Some("x"));
    }

    #[test]
    fn use_unused_helpers() {
        // Exercise helpers that aren't yet exercised elsewhere so the
        // unused-import warning doesn't fire.
        let parsed = crate::parse("test.kt", "fun f() = 1");
        let file = parsed.file();
        let node = file.syntax();
        let _ = children_of_kind(node, SyntaxKind::FUN).count();
        let _ = first_child_of_kind(node, SyntaxKind::FUN);
        let _ = first_typed_token::<KtIdentifier>(node);
    }

    #[test]
    fn fun_modifiers_and_visibility() {
        let src = "private inline suspend fun foo(): Int = 1";
        let parsed = crate::parse("t.kt", src);
        let file = parsed.file();
        let f = match file.decls().next().unwrap() {
            KtDecl::Fun(f) => f,
            _ => panic!("expected fun"),
        };
        use skotch_syntax::Visibility;
        assert_eq!(f.visibility(), Visibility::Private);
        assert!(f.is_inline());
        assert!(f.is_suspend());
        assert!(!f.is_open());
    }

    #[test]
    fn class_modifiers() {
        let src = "data class P(val x: Int)";
        let parsed = crate::parse("t.kt", src);
        let file = parsed.file();
        let c = match file.decls().next().unwrap() {
            KtDecl::Class(c) => c,
            _ => panic!("expected class"),
        };
        assert!(c.is_data());
        assert!(!c.is_open());
    }

    #[test]
    fn user_type_dotted_name() {
        let src = "fun f(): kotlin.collections.List = TODO()";
        let parsed = crate::parse("t.kt", src);
        let file = parsed.file();
        let f = match file.decls().next().unwrap() {
            KtDecl::Fun(f) => f,
            _ => panic!(),
        };
        let ret = f.return_type().expect("return type");
        let ut = ret.user_type().expect("user type");
        assert_eq!(ut.dotted_name(), "kotlin.collections.List");
    }

    #[test]
    fn function_type_return_type() {
        let src = "val f: (Int) -> String = TODO()";
        let parsed = crate::parse("t.kt", src);
        let file = parsed.file();
        let p = match file.decls().next().unwrap() {
            KtDecl::Property(p) => p,
            _ => panic!(),
        };
        let ty = p.type_reference().expect("type ref");
        let ft = ty.function_type().expect("function type");
        let ret = ft.return_type().expect("return ty");
        assert_eq!(
            ret.user_type().and_then(|u| u.name()),
            Some("String"),
        );
    }

    fn dump(n: &SilNode, depth: usize) {
        let indent = "  ".repeat(depth);
        match &n.data {
            SilData::Token { text } => println!("{}{:?} {:?}", indent, n.kind, text),
            SilData::Composite { children: cs } => {
                println!("{}{:?}", indent, n.kind);
                for c in cs {
                    dump(c, depth + 1);
                }
            }
            _ => println!("{}<ERR>", indent),
        }
    }

    #[test]
    #[ignore]
    fn debug_dump_user_type() {
        let parsed = crate::parse(
            "t.kt",
            "fun f(): kotlin.collections.List = TODO()",
        );
        dump(parsed.file().syntax(), 0);
    }

    #[test]
    #[ignore]
    fn debug_dump_function_type() {
        let parsed = crate::parse("t.kt", "val f: (Int) -> String = TODO()");
        dump(parsed.file().syntax(), 0);
    }

    #[test]
    #[ignore]
    fn debug_dump_nullable() {
        let parsed = crate::parse("t.kt", "fun f(x: Int?) {}");
        dump(parsed.file().syntax(), 0);
    }

    #[test]
    #[ignore]
    fn debug_dump_try_catch() {
        let parsed = crate::parse(
            "t.kt",
            "fun main() { try { println(\"hi\") } catch (e: Exception) { println(e) } }",
        );
        dump(parsed.file().syntax(), 0);
    }

    #[test]
    #[ignore]
    fn debug_dump_fun_body_with_locals() {
        let parsed = crate::parse(
            "t.kt",
            "fun main() {\n  val a: Int = 1\n  val b: String = \"hi\"\n}",
        );
        dump(parsed.file().syntax(), 0);
    }

    #[test]
    #[ignore]
    fn debug_dump_enum_iface_pkg() {
        for s in [
            "enum class Color { RED }",
            "interface Foo",
            "package com.foo\nclass Bar",
        ] {
            println!("=== {s:?}");
            let parsed = crate::parse("t.kt", s);
            dump(parsed.file().syntax(), 0);
        }
    }

    #[test]
    fn nullable_type_unwrap() {
        let src = "fun f(x: Int?) {}";
        let parsed = crate::parse("t.kt", src);
        let file = parsed.file();
        let f = match file.decls().next().unwrap() {
            KtDecl::Fun(f) => f,
            _ => panic!(),
        };
        let plist = f.value_parameter_list().expect("plist");
        let p = plist.parameters().next().expect("p");
        let ty = p.type_reference().expect("ty");
        assert!(ty.is_nullable());
        let inner = ty.nullable_type().unwrap().inner_user_type().unwrap();
        assert_eq!(inner.name(), Some("Int"));
    }
}
