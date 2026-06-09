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
ast_node!(KtClass = CLASS);
ast_node!(KtInterface = INTERFACE);
ast_node!(KtObjectDeclaration = OBJECT_DECLARATION);
ast_node!(KtCompanionObject = COMPANION_OBJECT);
ast_node!(KtEnumClass = ENUM_CLASS);
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
            SyntaxKind::CLASS => Some(Self::Class(KtClass::cast(node)?)),
            SyntaxKind::INTERFACE => Some(Self::Interface(KtInterface::cast(node)?)),
            SyntaxKind::OBJECT_DECLARATION => Some(Self::Object(KtObjectDeclaration::cast(node)?)),
            SyntaxKind::ENUM_CLASS => Some(Self::EnumClass(KtEnumClass::cast(node)?)),
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
    /// The dotted name as a contiguous string (no whitespace).
    pub fn name(self) -> String {
        let mut s = String::new();
        for c in children(self.syntax()) {
            if matches!(c.kind, SyntaxKind::IDENTIFIER | SyntaxKind::DOT) {
                if let SilData::Token { text } = &c.data {
                    s.push_str(text);
                }
            }
        }
        s
    }

    pub fn is_empty(self) -> bool {
        children(self.syntax()).is_empty()
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
}
