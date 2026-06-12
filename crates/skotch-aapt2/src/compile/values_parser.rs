//! Parser for `res/values/` XML documents.
//!
//! Port of aapt2's `ResourceParser.{h,cpp}`: turns a `<resources>`
//! document into [`ResourceTable`] entries — items (string/bool/color/
//! dimen/…), bags (attr/style/declare-styleable/array/plurals), and the
//! visibility/ID directives (public, public-group, staging-public-group,
//! java-symbol/symbol, add-resource, overlayable, macro).
//!
//! The C++ implementation streams over an `xml::XmlPullParser`; this
//! port walks the already-parsed DOM from [`crate::xml::parse_source_xml`]
//! instead. Where the C++ tracks comment state across pull events, the
//! DOM already attaches each comment to the element that follows it
//! (`Element::comment`).

use std::collections::HashMap;

use crate::res::config::ConfigDescription;
use crate::res::table::{
    policy, AllowNew, NewResource, Overlayable, OverlayableItem, ResourceTable, StagedId, Source2,
    Visibility, VisibilityLevel,
};
use crate::res::utils::{
    make_null, parse_bool, parse_resource_id, parse_style_parent_reference,
    parse_xml_attribute_name, string_to_int, try_parse_item_for_attribute,
};
use crate::res::value::{
    format, Array, Attribute, AttributeSymbol, Item, ItemValue, Macro, MacroNamespace, Plural,
    Reference, Span, Style, StyleEntry, StyleString, Styleable, UntranslatableSection, Value,
    ValueKind, PLURAL_FEW, PLURAL_MANY, PLURAL_ONE, PLURAL_OTHER, PLURAL_TWO, PLURAL_ZERO,
};
use crate::res::{
    FeatureFlagAttribute, FlagStatus, ResourceId, ResourceName, ResourceNamedType, ResourceType,
    Source,
};
use crate::util::{trim_whitespace, StringBuilder};
use crate::xml::{extract_package_from_namespace, Element, Node, XmlResource, SCHEMA_ANDROID};

/// The XLIFF 1.2 namespace, whose `<g>` tag marks untranslatable runs.
const XLIFF_NS_URI: &str = "urn:oasis:names:tc:xliff:document:1.2";

/// A feature flag's compile-time properties (`--feature-flags` values).
/// Mirrors `aapt::FeatureFlagProperties`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureFlagProperties {
    pub read_only: bool,
    pub enabled: Option<bool>,
}

/// Options controlling values parsing. Mirrors `aapt::ResourceParserOptions`.
#[derive(Debug, Clone)]
pub struct ResourceParserOptions {
    /// Whether the default setting for this parser is to allow translation.
    pub translatable: bool,
    /// Whether positional arguments in formatted strings are treated as
    /// errors or warnings (`--legacy` clears this).
    pub error_on_positional_arguments: bool,
    /// If true, apply the same visibility rules for styleables as are used
    /// for all other resources. Otherwise, all styleables will be made
    /// public (`--preserve-visibility-of-styleables` sets this).
    pub preserve_visibility_of_styleables: bool,
    /// If visibility was forced (`--visibility`), it is applied to all
    /// parsed resources, and the `<public>`, `<public-group>`,
    /// `<java-symbol>` and `<symbol>` tags are rejected.
    pub visibility: Option<VisibilityLevel>,
    /// Feature flag values from `--feature-flags`.
    pub feature_flags: HashMap<String, FeatureFlagProperties>,
    /// The flag (from the file path) that applies to all resources parsed.
    pub flag: Option<FeatureFlagAttribute>,
    /// Status of [`Self::flag`] under [`Self::feature_flags`].
    pub flag_status: FlagStatus,
}

impl Default for ResourceParserOptions {
    fn default() -> Self {
        ResourceParserOptions {
            translatable: true,
            error_on_positional_arguments: true,
            preserve_visibility_of_styleables: false,
            visibility: None,
            feature_flags: HashMap::new(),
            flag: None,
            flag_status: FlagStatus::NoFlag,
        }
    }
}

/// A flattened XML subtree: the raw and processed text of an element body
/// plus span/untranslatable bookkeeping. Mirrors `aapt::FlattenedXmlSubTree`.
#[derive(Debug, Clone, Default)]
pub struct FlattenedXmlSubTree {
    /// The unprocessed, concatenated text of the subtree.
    pub raw_value: String,
    /// The escaped/whitespace-processed text plus style spans.
    pub style_string: StyleString,
    pub untranslatable_sections: Vec<UntranslatableSection>,
    /// The `(prefix, uri)` namespace declarations in scope, outermost
    /// first. Stands in for the C++ `IPackageDeclStack`.
    pub namespace_stack: Vec<(String, String)>,
    pub source: Source,
}

/// A parsed resource ready to be added to the table.
/// Mirrors the C++ `ParsedResource`.
#[derive(Debug, Default)]
struct ParsedResource {
    name: ResourceName,
    config: ConfigDescription,
    product: String,
    source: Source,

    id: Option<ResourceId>,
    visibility_level: VisibilityLevel,
    staged_api: bool,
    allow_new: bool,
    overlayable_item: Option<OverlayableItem>,
    staged_alias: Option<StagedId>,
    flag: Option<FeatureFlagAttribute>,
    flag_status: FlagStatus,

    comment: String,
    value: Option<Value>,
    child_resources: Vec<ParsedResource>,
}

/// Parses a `res/values/` XML document into a [`ResourceTable`].
/// Port of `aapt::ResourceParser`.
pub struct ResourceParser<'a> {
    table: &'a mut ResourceTable,
    source: Source,
    config: ConfigDescription,
    options: ResourceParserOptions,
    errors: Vec<String>,
    /// Non-fatal diagnostics (the C++ `diag->Warn` calls).
    pub warnings: Vec<String>,
}

impl<'a> ResourceParser<'a> {
    pub fn new(
        table: &'a mut ResourceTable,
        source: Source,
        config: ConfigDescription,
        options: ResourceParserOptions,
    ) -> ResourceParser<'a> {
        ResourceParser { table, source, config, options, errors: Vec::new(), warnings: Vec::new() }
    }

    /// Parses a whole values document. The root element must be
    /// `<resources>`. Errors are accumulated and returned together, each
    /// prefixed with `path:line`.
    pub fn parse(&mut self, doc: &XmlResource) -> Result<(), Vec<String>> {
        let mut error = false;
        match &doc.root {
            Some(root) if root.namespace_uri.is_empty() && root.name == "resources" => {
                let mut ns_stack: Vec<(String, String)> = Vec::new();
                push_decls(&mut ns_stack, root);
                if !self.parse_resources(root, &mut ns_stack) {
                    error = true;
                }
            }
            Some(root) => {
                let source = self.source_at(root);
                self.error(&source, "root element must be <resources>");
                error = true;
            }
            None => {
                let source = self.source.clone();
                self.error(&source, "root element must be <resources>");
                error = true;
            }
        }
        if error || !self.errors.is_empty() {
            let mut errors = std::mem::take(&mut self.errors);
            if errors.is_empty() {
                errors.push(format!("{}: failed to parse resources", self.source));
            }
            Err(errors)
        } else {
            Ok(())
        }
    }

    // ─────────────────────────── diagnostics ───────────────────────────

    fn source_at(&self, el: &Element) -> Source {
        Source::with_line(self.source.path.clone(), el.line_number)
    }

    fn error(&mut self, source: &Source, msg: &str) {
        self.errors.push(format!("{source}: {msg}"));
    }

    fn error_at(&mut self, el: &Element, msg: &str) {
        let source = self.source_at(el);
        self.error(&source, msg);
    }

    fn warn(&mut self, source: &Source, msg: &str) {
        self.warnings.push(format!("{source}: {msg}"));
    }

    // ──────────────────────── <resources> scan ─────────────────────────

    /// Port of `ResourceParser::ParseResources`.
    fn parse_resources(&mut self, root: &Element, ns_stack: &mut Vec<(String, String)>) -> bool {
        let mut error = false;
        for child in &root.children {
            match child {
                Node::Text(text) => {
                    if !trim_whitespace(&text.text).is_empty() {
                        let source =
                            Source::with_line(self.source.path.clone(), text.line_number);
                        self.error(&source, "plain text not allowed here");
                        error = true;
                    }
                }
                Node::Element(el) => {
                    if !el.namespace_uri.is_empty() {
                        // Skip unknown namespace.
                        continue;
                    }
                    if el.name == "skip" || el.name == "eat-comment" {
                        continue;
                    }

                    let pushed = push_decls(ns_stack, el);

                    let mut parsed = ParsedResource {
                        config: self.config.clone(),
                        source: self.source_at(el),
                        comment: el.comment.clone(),
                        ..Default::default()
                    };
                    if let Some(visibility) = self.options.visibility {
                        parsed.visibility_level = visibility;
                    }
                    // Extract the product name if it exists.
                    if let Some(product) = find_non_empty_attr(el, "product") {
                        parsed.product = product.to_string();
                    }

                    if !self.parse_resource(el, ns_stack, &mut parsed) {
                        error = true;
                    } else if !self.add_resources_to_table(parsed) {
                        error = true;
                    }

                    pop_decls(ns_stack, pushed);
                }
            }
        }
        !error
    }

    /// Recursively adds a parsed resource (and its children) to the table.
    /// Port of the C++ `AddResourcesToTable`.
    fn add_resources_to_table(&mut self, res: ParsedResource) -> bool {
        let comment = trim_whitespace(&res.comment).to_string();

        if !res.name.entry.is_empty() {
            let mut new_res = NewResource::with_name(res.name.clone())
                .config(res.config.clone())
                .product(res.product.clone());

            if res.visibility_level != VisibilityLevel::Undefined {
                new_res = new_res.visibility(Visibility {
                    level: res.visibility_level,
                    staged_api: res.staged_api,
                    source: res.source.clone(),
                    comment: comment.clone(),
                });
            }
            if let Some(id) = res.id {
                new_res = new_res.id(id);
            }
            if res.allow_new {
                new_res = new_res.allow_new(AllowNew {
                    source: res.source.clone(),
                    comment: comment.clone(),
                });
            }
            if let Some(overlayable) = res.overlayable_item.clone() {
                new_res = new_res.overlayable(overlayable);
            }
            if let Some(mut value) = res.value {
                value.meta.flag = res.flag.clone();
                value.meta.flag_status = res.flag_status;
                // Attach the comment, source and config to the value.
                value.meta.comment = comment;
                value.meta.source = res.source.clone();
                new_res = new_res.value(value);
            }
            if let Some(staged_alias) = res.staged_alias {
                new_res = new_res.staged_id(staged_alias);
            }

            if let Err(err) = self.table.add_resource(new_res) {
                self.errors.push(err.to_string());
                return false;
            }
        }

        let mut error = false;
        for child in res.child_resources {
            error |= !self.add_resources_to_table(child);
        }
        !error
    }

    // ───────────────────────── element dispatch ────────────────────────

    /// Port of `ResourceParser::ParseResource`.
    fn parse_resource(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
    ) -> bool {
        let mut resource_type: String = el.name.clone();

        // android:featureFlag handling.
        if let Some(flag) = parse_flag(find_attr_ns(el, SCHEMA_ANDROID, "featureFlag")) {
            if self.options.flag.is_some() {
                self.error_at(el, "Resource flag are not allowed both in the path and in the file");
                return false;
            }
            match get_flag_status(&Some(flag.clone()), &self.options.feature_flags) {
                Ok(status) => {
                    out.flag = Some(flag);
                    out.flag_status = status;
                }
                Err(err) => {
                    self.error_at(el, &err);
                    return false;
                }
            }
        } else if self.options.flag.is_some() {
            out.flag = self.options.flag.clone();
            out.flag_status = self.options.flag_status;
        }

        // The value format accepted for this resource.
        let mut resource_format = 0u32;

        let mut can_be_item = true;
        let mut can_be_bag = true;
        if resource_type == "item" {
            can_be_bag = false;

            // The default format for <item> is any. An explicit format
            // attribute overrides it.
            resource_format = format::ANY;

            // Items have their type encoded in the type attribute.
            match find_non_empty_attr(el, "type") {
                Some(ty) => resource_type = ty.to_string(),
                None => {
                    self.error_at(el, "<item> must have a 'type' attribute");
                    return false;
                }
            }

            if let Some(format_str) = find_non_empty_attr(el, "format") {
                resource_format = parse_format_type_no_enums_or_flags(format_str);
                if resource_format == 0 {
                    let source = out.source.clone();
                    self.error(&source, &format!("'{format_str}' is an invalid format"));
                    return false;
                }
            }
        } else if resource_type == "bag" {
            can_be_item = false;

            // Bags have their type encoded in the type attribute.
            match find_non_empty_attr(el, "type") {
                Some(ty) => resource_type = ty.to_string(),
                None => {
                    self.error_at(el, "<bag> must have a 'type' attribute");
                    return false;
                }
            }
        }

        // The name will be checked later, because not all XML elements
        // require a name.
        let maybe_name = find_non_empty_attr(el, "name").map(str::to_string);

        if resource_type == "id" {
            let Some(name) = &maybe_name else {
                let source = out.source.clone();
                self.error(&source, &format!("<{}> missing 'name' attribute", el.name));
                return false;
            };
            out.name = ResourceName::new("", ResourceType::Id, name.clone());

            // Ids either represent a unique resource id or reference another
            // resource id.
            if !self.parse_item(el, ns_stack, out, resource_format) {
                return false;
            }

            enum IdShape {
                MakeId,
                Keep,
                Invalid,
            }
            let shape = match out.value.as_ref().map(|v| &v.kind) {
                // If no inner element exists, represent a unique identifier.
                Some(ValueKind::Item(Item::String { value, .. })) if value.is_empty() => {
                    IdShape::MakeId
                }
                // A null reference also means there is no inner element
                // (<id name="name"/>).
                Some(ValueKind::Item(Item::Reference(r)))
                    if r.name.is_none() && r.id.is_none() =>
                {
                    IdShape::MakeId
                }
                // An existing inner element must be a reference to another
                // resource id.
                Some(ValueKind::Item(Item::Reference(r)))
                    if r.name.as_ref().is_some_and(|n| n.ty.ty == ResourceType::Id) =>
                {
                    IdShape::Keep
                }
                _ => IdShape::Invalid,
            };
            match shape {
                IdShape::MakeId => out.value = Some(Value::item(Item::Id)),
                IdShape::Keep => {}
                IdShape::Invalid => {
                    let source = out.source.clone();
                    self.error(
                        &source,
                        &format!(
                            "<{}> inner element must either be a resource reference or empty",
                            el.name
                        ),
                    );
                    return false;
                }
            }
            return true;
        } else if resource_type == "macro" {
            let Some(name) = &maybe_name else {
                let source = out.source.clone();
                self.error(&source, &format!("<{}> missing 'name' attribute", el.name));
                return false;
            };
            out.name = ResourceName::new("", ResourceType::Macro, name.clone());
            return self.parse_macro(el, ns_stack, out);
        }

        if can_be_item {
            if let Some((ty, implied_format)) = item_type_format(&resource_type) {
                // This is an item; record its type and format and start
                // parsing.
                let Some(name) = &maybe_name else {
                    let source = out.source.clone();
                    self.error(&source, &format!("<{}> missing 'name' attribute", el.name));
                    return false;
                };
                out.name = ResourceName::new("", ty, name.clone());

                // Only use the implied format of the type when there is no
                // explicit format.
                if resource_format == 0 {
                    resource_format = implied_format;
                }
                return self.parse_item(el, ns_stack, out, resource_format);
            }
        }

        // This might be a bag or something.
        if can_be_bag {
            if let Some(bag_kind) = bag_kind(&resource_type) {
                // Ensure we have a name (unless this is a <public-group> /
                // <staging-public-group[-final]> or <overlayable>).
                if !matches!(
                    bag_kind,
                    BagKind::PublicGroup
                        | BagKind::StagingPublicGroup
                        | BagKind::StagingPublicGroupFinal
                        | BagKind::Overlayable
                ) {
                    let Some(name) = &maybe_name else {
                        let source = out.source.clone();
                        self.error(&source, &format!("<{}> missing 'name' attribute", el.name));
                        return false;
                    };
                    out.name.entry = name.clone();
                }

                return match bag_kind {
                    BagKind::AddResource => self.parse_add_resource(el, out),
                    BagKind::Array => self.parse_array(el, ns_stack, out),
                    BagKind::Attr => self.parse_attr_impl(el, out, false),
                    BagKind::ConfigVarying => {
                        self.parse_style(ResourceType::ConfigVarying, el, ns_stack, out)
                    }
                    BagKind::DeclareStyleable => self.parse_declare_styleable(el, ns_stack, out),
                    BagKind::IntegerArray => {
                        self.parse_array_impl(el, ns_stack, out, format::INTEGER)
                    }
                    BagKind::Symbol => self.parse_symbol(el, out),
                    BagKind::Overlayable => self.parse_overlayable(el, out),
                    BagKind::Plurals => self.parse_plural(el, ns_stack, out),
                    BagKind::Public => self.parse_public(el, out),
                    BagKind::PublicGroup => self.parse_public_group(el, out),
                    BagKind::StagingPublicGroup => self.parse_staging_public_group(el, out),
                    BagKind::StagingPublicGroupFinal => {
                        self.parse_staging_public_group_final(el, out)
                    }
                    BagKind::StringArray => self.parse_array_impl(el, ns_stack, out, format::STRING),
                    BagKind::Style => self.parse_style(ResourceType::Style, el, ns_stack, out),
                };
            }
        }

        if can_be_item {
            // Try parsing the elementName (or type) as a resource. These
            // shall only be resources like 'layout' or 'xml' and they can
            // only be references.
            if let Some(parsed_type) = ResourceNamedType::parse(&resource_type) {
                let Some(name) = &maybe_name else {
                    let source = out.source.clone();
                    self.error(&source, &format!("<{}> missing 'name' attribute", el.name));
                    return false;
                };
                out.name =
                    ResourceName::with_named_type("", parsed_type.clone(), name.clone());
                match self.parse_xml(el, ns_stack, format::REFERENCE, false) {
                    Some(item) => {
                        out.value = Some(Value::item(item));
                        return true;
                    }
                    None => {
                        let source = out.source.clone();
                        self.error(
                            &source,
                            &format!(
                                "invalid value for type '{parsed_type}'. Expected a reference"
                            ),
                        );
                        return false;
                    }
                }
            }
        }

        // If the resource type was not recognized, write the error and
        // return false.
        let source = out.source.clone();
        self.error(&source, &format!("unknown resource type '{resource_type}'"));
        false
    }

    // ─────────────────────────── items ─────────────────────────────────

    /// Port of `ResourceParser::ParseItem`.
    fn parse_item(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
        item_format: u32,
    ) -> bool {
        if item_format == format::STRING {
            return self.parse_string(el, ns_stack, out);
        }
        match self.parse_xml(el, ns_stack, item_format, false) {
            Some(item) => {
                out.value = Some(Value::item(item));
                true
            }
            None => {
                let source = out.source.clone();
                self.error(&source, &format!("invalid {}", out.name.ty));
                false
            }
        }
    }

    /// Reads the entire XML subtree and attempts to parse it as some Item
    /// whose type is allowed by `type_mask`. If `allow_raw_value` is true
    /// and the subtree can not be parsed as a regular Item, a RawString is
    /// returned. Port of the member `ResourceParser::ParseXml`.
    fn parse_xml(
        &mut self,
        el: &Element,
        ns_stack: &[(String, String)],
        type_mask: u32,
        allow_raw_value: bool,
    ) -> Option<Item> {
        let sub_tree = self.flatten_subtree(el, ns_stack)?;
        Self::parse_flattened_xml(&sub_tree, type_mask, allow_raw_value, self.table, &mut self.errors)
    }

    /// Static counterpart usable outside a full parse (macro substitution
    /// during linking). Port of the static `ResourceParser::ParseXml`.
    pub fn parse_flattened_xml(
        sub_tree: &FlattenedXmlSubTree,
        type_mask: u32,
        allow_raw_value: bool,
        table: &mut ResourceTable,
        errors: &mut Vec<String>,
    ) -> Option<Item> {
        if !sub_tree.style_string.spans.is_empty() {
            // This can only be a StyledString.
            return Some(Item::StyledString {
                value: sub_tree.style_string.str.clone(),
                spans: sub_tree
                    .style_string
                    .spans
                    .iter()
                    .map(|span| Span {
                        name: span.name.clone(),
                        first_char: span.first_char,
                        last_char: span.last_char,
                    })
                    .collect(),
                untranslatable_sections: sub_tree.untranslatable_sections.clone(),
            });
        }

        let source = sub_tree.source.clone();
        let mut on_create_reference = |name: &ResourceName| -> bool {
            // name.package can be empty here; it will assume the package
            // name of the table.
            let mut id_value = Value::item(Item::Id);
            id_value.meta.source = source.clone();
            match table.add_resource(NewResource::with_name(name.clone()).value(id_value)) {
                Ok(()) => true,
                Err(err) => {
                    errors.push(err.to_string());
                    false
                }
            }
        };

        // Process the raw value.
        if let Some(mut item) = try_parse_item_for_attribute(
            &sub_tree.raw_value,
            type_mask,
            Some(&mut on_create_reference),
        ) {
            // Fix up the reference.
            if let Item::Reference(reference) = &mut item {
                reference.allow_raw = allow_raw_value;
                resolve_package(&sub_tree.namespace_stack, reference);
            }
            return Some(item);
        }

        // Try making a regular string.
        if type_mask & format::STRING != 0 {
            // Use the trimmed, escaped string.
            return Some(Item::String {
                value: sub_tree.style_string.str.clone(),
                untranslatable_sections: sub_tree.untranslatable_sections.clone(),
            });
        }

        if allow_raw_value {
            // We can't parse this so return a RawString if we are allowed.
            return Some(Item::RawString(trim_whitespace(&sub_tree.raw_value).to_string()));
        }
        if trim_whitespace(&sub_tree.raw_value).is_empty() {
            // If the text is empty, and the value is not allowed to be a
            // string, encode it as a @null.
            return Some(make_null());
        }
        None
    }

    /// Port of `ResourceParser::ParseString`.
    fn parse_string(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
    ) -> bool {
        let mut formatted = true;
        if let Some(formatted_attr) = find_attr(el, "formatted") {
            match parse_bool(formatted_attr) {
                Some(value) => formatted = value,
                None => {
                    let source = out.source.clone();
                    self.error(&source, "invalid value for 'formatted'. Must be a boolean");
                    return false;
                }
            }
        }

        let mut translatable = self.options.translatable;
        if let Some(translatable_attr) = find_attr(el, "translatable") {
            match parse_bool(translatable_attr) {
                Some(value) => translatable = value,
                None => {
                    let source = out.source.clone();
                    self.error(&source, "invalid value for 'translatable'. Must be a boolean");
                    return false;
                }
            }
        }

        let Some(item) = self.parse_xml(el, ns_stack, format::STRING, false) else {
            let source = out.source.clone();
            self.error(&source, "not a valid string");
            return false;
        };

        let mut value = Value::item(item);
        let plain_string = match &value.kind {
            ValueKind::Item(Item::String { value: text, .. }) => Some(text.clone()),
            _ => None,
        };
        let is_styled = matches!(&value.kind, ValueKind::Item(Item::StyledString { .. }));
        if plain_string.is_some() || is_styled {
            value.meta.translatable = translatable;
        }
        if let Some(text) = plain_string {
            if formatted && translatable && !verify_java_string_format(&text) {
                let msg = "multiple substitutions specified in non-positional format; \
                           did you mean to add the formatted=\"false\" attribute?";
                if self.options.error_on_positional_arguments {
                    let source = out.source.clone();
                    self.error(&source, msg);
                    return false;
                }
                let source = out.source.clone();
                self.warn(&source, msg);
            }
        }
        out.value = Some(value);
        true
    }

    // ─────────────────────────── macro ─────────────────────────────────

    /// Port of `ResourceParser::ParseMacro`.
    fn parse_macro(
        &mut self,
        el: &Element,
        ns_stack: &[(String, String)],
        out: &mut ParsedResource,
    ) -> bool {
        let Some(sub_tree) = self.flatten_subtree(el, ns_stack) else {
            return false;
        };

        if !out.config.is_default() {
            let source = out.source.clone();
            self.error(
                &source,
                "<macro> tags cannot be declared in configurations other than the default \
                 configuration'",
            );
            return false;
        }

        let mut value = Macro {
            raw_value: sub_tree.raw_value,
            style_string: sub_tree.style_string,
            untranslatable_sections: sub_tree.untranslatable_sections,
            alias_namespaces: Vec::new(),
        };
        for (prefix, uri) in &sub_tree.namespace_stack {
            if let Some(package) = extract_package_from_namespace(uri) {
                value.alias_namespaces.push(MacroNamespace {
                    alias: prefix.clone(),
                    package_name: package.package,
                    is_private: package.private_namespace,
                });
            }
        }
        out.value = Some(Value::new(ValueKind::Macro(value)));
        true
    }

    // ──────────────────── visibility / ID directives ───────────────────

    /// Port of `ResourceParser::ParsePublic`.
    fn parse_public(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        if self.options.visibility.is_some() {
            let source = out.source.clone();
            self.error(&source, "<public> tag not allowed with --visibility flag");
            return false;
        }
        if !out.config.is_default() {
            let source = out.source.clone();
            let config = out.config.clone();
            self.warn(&source, &format!("ignoring configuration '{config}' for <public> tag"));
        }

        let Some(type_str) = find_non_empty_attr(el, "type") else {
            let source = out.source.clone();
            self.error(&source, "<public> must have a 'type' attribute");
            return false;
        };
        let Some(parsed_type) = ResourceNamedType::parse(type_str) else {
            let source = out.source.clone();
            self.error(&source, &format!("invalid resource type '{type_str}' in <public>"));
            return false;
        };
        out.name.ty = parsed_type.clone();

        if let Some(id_str) = find_non_empty_attr(el, "id") {
            match parse_resource_id(id_str) {
                Some(id) => out.id = Some(id),
                None => {
                    let source = out.source.clone();
                    self.error(&source, &format!("invalid resource ID '{id_str}' in <public>"));
                    return false;
                }
            }
        }

        if parsed_type.ty == ResourceType::Id {
            // An ID marked as public is also the definition of an ID.
            out.value = Some(Value::item(Item::Id));
        }
        out.visibility_level = VisibilityLevel::Public;
        true
    }

    /// Shared body of the `<public-group>`-shaped tags.
    /// Port of the C++ `ParseGroupImpl`.
    fn parse_group_impl(
        &mut self,
        el: &Element,
        out: &mut ParsedResource,
        tag_name: &str,
        apply: &dyn Fn(&mut ParsedResource, ResourceId),
    ) -> bool {
        if !out.config.is_default() {
            let source = out.source.clone();
            let config = out.config.clone();
            self.warn(
                &source,
                &format!("ignoring configuration '{config}' for <{tag_name}> tag"),
            );
        }

        let Some(type_str) = find_non_empty_attr(el, "type") else {
            let source = out.source.clone();
            self.error(&source, &format!("<{tag_name}> must have a 'type' attribute"));
            return false;
        };
        let Some(parsed_type) = ResourceNamedType::parse(type_str) else {
            let source = out.source.clone();
            self.error(
                &source,
                &format!("invalid resource type '{type_str}' in <{tag_name}>"),
            );
            return false;
        };

        let Some(id_str) = find_non_empty_attr(el, "first-id") else {
            let source = out.source.clone();
            self.error(&source, &format!("<{tag_name}> must have a 'first-id' attribute"));
            return false;
        };
        let Some(first_id) = parse_resource_id(id_str) else {
            let source = out.source.clone();
            self.error(&source, &format!("invalid resource ID '{id_str}' in <{tag_name}>"));
            return false;
        };

        let mut next_id = first_id.0;
        let mut error = false;
        for child in el.child_elements() {
            let item_source = self.source_at(child);
            if child.namespace_uri.is_empty() && child.name == "public" {
                let Some(name) = find_non_empty_attr(child, "name") else {
                    self.error(&item_source, "<public> must have a 'name' attribute");
                    error = true;
                    continue;
                };
                if find_non_empty_attr(child, "id").is_some() {
                    self.error(&item_source, &format!("'id' is ignored within <{tag_name}>"));
                    error = true;
                    continue;
                }
                if find_non_empty_attr(child, "type").is_some() {
                    self.error(&item_source, &format!("'type' is ignored within <{tag_name}>"));
                    error = true;
                    continue;
                }
                if name.starts_with("removed_") {
                    // Skip resources that have been removed from the
                    // framework, but leave a hole so that other staged
                    // resources don't shift and break apps previously
                    // compiled against them.
                    next_id = next_id.wrapping_add(1);
                    continue;
                }

                let mut entry_res = ParsedResource {
                    name: ResourceName::with_named_type("", parsed_type.clone(), name),
                    source: item_source,
                    comment: child.comment.clone(),
                    ..Default::default()
                };
                // Execute group specific code.
                apply(&mut entry_res, ResourceId(next_id));
                out.child_resources.push(entry_res);

                next_id = next_id.wrapping_add(1);
            } else if !should_ignore_element(child) {
                self.error(&item_source, &format!(":{}>", child.name));
                error = true;
            }
        }
        !error
    }

    /// Port of `ResourceParser::ParsePublicGroup`.
    fn parse_public_group(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        if self.options.visibility.is_some() {
            let source = out.source.clone();
            self.error(&source, "<public-group> tag not allowed with --visibility flag");
            return false;
        }
        self.parse_group_impl(el, out, "public-group", &|entry, id| {
            entry.id = Some(id);
            entry.visibility_level = VisibilityLevel::Public;
        })
    }

    /// Port of `ResourceParser::ParseStagingPublicGroup`.
    fn parse_staging_public_group(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        self.parse_group_impl(el, out, "staging-public-group", &|entry, id| {
            entry.id = Some(id);
            entry.staged_api = true;
            entry.visibility_level = VisibilityLevel::Public;
        })
    }

    /// Port of `ResourceParser::ParseStagingPublicGroupFinal`.
    fn parse_staging_public_group_final(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        self.parse_group_impl(el, out, "staging-public-group-final", &|entry, id| {
            entry.staged_alias = Some(StagedId { id, source: Source2 });
        })
    }

    /// Port of `ResourceParser::ParseSymbolImpl`.
    fn parse_symbol_impl(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        let Some(type_str) = find_non_empty_attr(el, "type") else {
            let source = out.source.clone();
            self.error(&source, &format!("<{}> must have a 'type' attribute", el.name));
            return false;
        };
        let Some(parsed_type) = ResourceNamedType::parse(type_str) else {
            let source = out.source.clone();
            self.error(
                &source,
                &format!("invalid resource type '{type_str}' in <{}>", el.name),
            );
            return false;
        };
        out.name.ty = parsed_type;
        true
    }

    /// Port of `ResourceParser::ParseSymbol` (`<java-symbol>`/`<symbol>`).
    fn parse_symbol(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        if self.options.visibility.is_some() {
            let source = out.source.clone();
            self.error(
                &source,
                "<java-symbol> and <symbol> tags not allowed with --visibility flag",
            );
            return false;
        }
        if !out.config.is_default() {
            let source = out.source.clone();
            let config = out.config.clone();
            self.warn(
                &source,
                &format!("ignoring configuration '{config}' for <{}> tag", el.name),
            );
        }
        if !self.parse_symbol_impl(el, out) {
            return false;
        }
        out.visibility_level = VisibilityLevel::Private;
        true
    }

    /// Port of `ResourceParser::ParseAddResource`.
    fn parse_add_resource(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        if self.parse_symbol_impl(el, out) {
            out.visibility_level = VisibilityLevel::Undefined;
            out.allow_new = true;
            return true;
        }
        false
    }

    // ───────────────────────── overlayable ─────────────────────────────

    /// Port of `ResourceParser::ParseOverlayable`.
    fn parse_overlayable(&mut self, el: &Element, out: &mut ParsedResource) -> bool {
        if !out.config.is_default() {
            let source = out.source.clone();
            let config = out.config.clone();
            self.warn(
                &source,
                &format!("ignoring configuration '{config}' for <overlayable> tag"),
            );
        }

        let Some(overlayable_name) = find_non_empty_attr(el, "name") else {
            let source = out.source.clone();
            self.error(&source, "<overlayable> tag must have a 'name' attribute");
            return false;
        };

        let overlayable_actor = find_non_empty_attr(el, "actor");
        if let Some(actor) = overlayable_actor {
            if !actor.starts_with(Overlayable::ACTOR_SCHEME_URI) {
                let source = out.source.clone();
                self.error(
                    &source,
                    &format!(
                        "specified <overlayable> tag 'actor' attribute must use the scheme '{}'",
                        Overlayable::ACTOR_SCHEME
                    ),
                );
                return false;
            }
        }

        // Create an overlayable entry grouping that represents this
        // <overlayable>.
        let overlayable_index = self.table.overlayables.len();
        self.table.overlayables.push(Overlayable {
            name: overlayable_name.to_string(),
            actor: overlayable_actor.unwrap_or("").to_string(),
            source: self.source.clone(),
        });

        let mut error = false;
        'outer: for child in el.child_elements() {
            let element_source = self.source_at(child);
            if child.namespace_uri.is_empty() && child.name == "item" {
                // <item> outside a <policy> block.
                self.error(
                    &element_source,
                    "<item> within an <overlayable> must be inside a <policy> block",
                );
                error = true;
            } else if child.namespace_uri.is_empty() && child.name == "policy" {
                // Parse the policies separated by vertical bar characters
                // to allow specifying multiple policies.
                let mut current_policies = policy::NONE;
                match find_non_empty_attr(child, "type") {
                    Some(type_str) => {
                        for part in type_str.split('|') {
                            let trimmed_part = trim_whitespace(part);
                            match policy_from_str(trimmed_part) {
                                Some(flags) => current_policies |= flags,
                                None => {
                                    self.error(
                                        &element_source,
                                        &format!(
                                            "<policy> has unsupported type '{trimmed_part}'"
                                        ),
                                    );
                                    error = true;
                                    continue;
                                }
                            }
                        }
                    }
                    None => {
                        self.error(&element_source, "<policy> must have a 'type' attribute");
                        error = true;
                        continue;
                    }
                }

                for item in child.child_elements() {
                    let item_source = self.source_at(item);
                    if item.namespace_uri.is_empty() && item.name == "item" {
                        if !self.parse_overlayable_item(
                            item,
                            current_policies,
                            overlayable_index,
                            out,
                        ) {
                            error = true;
                        }
                    } else if item.namespace_uri.is_empty() && item.name == "policy" {
                        self.error(&item_source, "<policy> blocks cannot be recursively nested");
                        error = true;
                        break 'outer;
                    } else if !should_ignore_element(item) {
                        self.error(
                            &item_source,
                            &format!("invalid element <{}>  in <overlayable>", item.name),
                        );
                        error = true;
                        break 'outer;
                    }
                }
            } else if !should_ignore_element(child) {
                self.error(
                    &element_source,
                    &format!("invalid element <{}>  in <overlayable>", child.name),
                );
                error = true;
                break;
            }
        }
        !error
    }

    /// One `<item type name>` inside an `<overlayable>` `<policy>` block.
    fn parse_overlayable_item(
        &mut self,
        el: &Element,
        policies: u32,
        overlayable_index: usize,
        out: &mut ParsedResource,
    ) -> bool {
        let item_source = self.source_at(el);

        // Items specify the name and type of resource that should be
        // overlayable.
        let Some(item_name) = find_non_empty_attr(el, "name") else {
            self.error(&item_source, "<item> within an <overlayable> must have a 'name' attribute");
            return false;
        };
        let Some(item_type) = find_non_empty_attr(el, "type") else {
            self.error(&item_source, "<item> within an <overlayable> must have a 'type' attribute");
            return false;
        };
        let Some(parsed_type) = ResourceNamedType::parse(item_type) else {
            self.error(
                &item_source,
                &format!("invalid resource type '{item_type}' in <item> within an <overlayable>"),
            );
            return false;
        };

        let child_resource = ParsedResource {
            name: ResourceName::with_named_type("", parsed_type, item_name),
            overlayable_item: Some(OverlayableItem {
                overlayable_index,
                policies,
                comment: el.comment.clone(),
                source: item_source,
            }),
            ..Default::default()
        };
        out.child_resources.push(child_resource);
        true
    }

    // ─────────────────────────── attributes ────────────────────────────

    /// Port of `ResourceParser::ParseAttrImpl` (`ParseAttr` is the
    /// `weak = false` call).
    fn parse_attr_impl(&mut self, el: &Element, out: &mut ParsedResource, weak: bool) -> bool {
        out.name.ty = ResourceNamedType::with_default_name(ResourceType::Attr);

        // Attributes only end up in default configuration.
        if !out.config.is_default() {
            let source = out.source.clone();
            let config = out.config.clone();
            let name = out.name.clone();
            self.warn(
                &source,
                &format!("ignoring configuration '{config}' for attribute {name}"),
            );
            out.config = ConfigDescription::default();
        }

        let mut type_mask = 0u32;
        if let Some(format_attr) = find_attr(el, "format") {
            type_mask = parse_format_attribute(format_attr);
            if type_mask == 0 {
                self.error_at(el, &format!("invalid attribute format '{format_attr}'"));
                return false;
            }
        }

        let mut maybe_min: Option<i32> = None;
        let mut maybe_max: Option<i32> = None;

        if let Some(min_str) = find_attr(el, "min") {
            let min_str = trim_whitespace(min_str);
            if !min_str.is_empty() {
                if let Some(value) = string_to_int(min_str) {
                    maybe_min = Some(value.data as i32);
                }
            }
            if maybe_min.is_none() {
                self.error_at(el, &format!("invalid 'min' value '{min_str}'"));
                return false;
            }
        }

        if let Some(max_str) = find_attr(el, "max") {
            let max_str = trim_whitespace(max_str);
            if !max_str.is_empty() {
                if let Some(value) = string_to_int(max_str) {
                    maybe_max = Some(value.data as i32);
                }
            }
            if maybe_max.is_none() {
                self.error_at(el, &format!("invalid 'max' value '{max_str}'"));
                return false;
            }
        }

        if (maybe_min.is_some() || maybe_max.is_some()) && type_mask & format::INTEGER == 0 {
            self.error_at(el, "'min' and 'max' can only be used when format='integer'");
            return false;
        }

        // Symbols are stored sorted by symbol name (the C++ uses a std::set
        // keyed on the symbol's resource name).
        let mut symbols: Vec<AttributeSymbol> = Vec::new();
        let mut error = false;
        for child in el.child_elements() {
            let item_source = self.source_at(child);
            let element_name = child.name.as_str();
            if child.namespace_uri.is_empty() && (element_name == "flag" || element_name == "enum")
            {
                if element_name == "enum" {
                    if type_mask & format::FLAGS != 0 {
                        self.error(&item_source, "can not define an <enum>; already defined a <flag>");
                        error = true;
                        continue;
                    }
                    type_mask |= format::ENUM;
                } else {
                    if type_mask & format::ENUM != 0 {
                        self.error(&item_source, "can not define a <flag>; already defined an <enum>");
                        error = true;
                        continue;
                    }
                    type_mask |= format::FLAGS;
                }

                match self.parse_enum_or_flag_item(child, element_name) {
                    Some(mut symbol) => {
                        let Some(symbol_name) = symbol.symbol.name.clone() else {
                            error = true;
                            continue;
                        };

                        let mut child_resource = ParsedResource {
                            name: symbol_name.clone(),
                            source: item_source.clone(),
                            value: Some(Value::item(Item::Id)),
                            ..Default::default()
                        };
                        if let Some(visibility) = self.options.visibility {
                            child_resource.visibility_level = visibility;
                        }
                        out.child_resources.push(child_resource);

                        symbol.comment = child.comment.clone();
                        symbol.source = item_source.clone();

                        let key = Some(symbol_name.clone());
                        match symbols.binary_search_by(|existing| existing.symbol.name.cmp(&key)) {
                            Ok(_) => {
                                self.error(
                                    &item_source,
                                    &format!("duplicate symbol '{}'", symbol_name.entry),
                                );
                                error = true;
                            }
                            Err(pos) => symbols.insert(pos, symbol),
                        }
                    }
                    None => error = true,
                }
            } else if !should_ignore_element(child) {
                self.error(&item_source, &format!(":{element_name}>"));
                error = true;
            }
        }

        if error {
            return false;
        }

        let mut attr = Attribute::new(if type_mask != 0 { type_mask } else { format::ANY });
        attr.symbols = symbols;
        attr.min_int = maybe_min.unwrap_or(i32::MIN);
        attr.max_int = maybe_max.unwrap_or(i32::MAX);

        let mut value = Value::new(ValueKind::Attribute(attr));
        value.meta.weak = weak;
        out.value = Some(value);
        true
    }

    /// Port of `ResourceParser::ParseEnumOrFlagItem`.
    fn parse_enum_or_flag_item(&mut self, el: &Element, tag: &str) -> Option<AttributeSymbol> {
        let source = self.source_at(el);

        let Some(name) = find_non_empty_attr(el, "name") else {
            self.error(&source, &format!("no attribute 'name' found for tag <{tag}>"));
            return None;
        };
        let Some(value_str) = find_non_empty_attr(el, "value") else {
            self.error(&source, &format!("no attribute 'value' found for tag <{tag}>"));
            return None;
        };
        let Some(value) = string_to_int(value_str) else {
            self.error(
                &source,
                &format!("invalid value '{value_str}' for <{tag}>; must be an integer"),
            );
            return None;
        };

        Some(AttributeSymbol {
            symbol: Reference::from_name(ResourceName::new("", ResourceType::Id, name)),
            source,
            comment: String::new(),
            value: value.data,
            data_type: value.data_type,
        })
    }

    // ───────────────────────────── styles ──────────────────────────────

    /// Port of `ResourceParser::ParseStyleItem`.
    fn parse_style_item(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        style: &mut Style,
    ) -> bool {
        let source = self.source_at(el);

        let Some(name) = find_non_empty_attr(el, "name") else {
            self.error(&source, "<item> must have a 'name' attribute");
            return false;
        };

        // If the name has a package, separate it out (e.g.
        // <item name="android:text">).
        let mut key = parse_xml_attribute_name(name);
        resolve_package(ns_stack, &mut key);

        let flag = parse_flag(find_attr_ns(el, SCHEMA_ANDROID, "featureFlag"));

        let Some(item) = self.parse_xml(el, ns_stack, 0, true) else {
            self.error(&source, "could not parse style item");
            return false;
        };
        let mut item_value = ItemValue::new(item);

        if let Some(flag) = flag {
            if self.options.flag.is_some() {
                self.error_at(el, "Resource flag are not allowed both in the path and in the file");
                return false;
            }
            match get_flag_status(&Some(flag.clone()), &self.options.feature_flags) {
                Ok(status) => {
                    item_value.meta.flag_status = status;
                    item_value.meta.flag = Some(flag);
                }
                Err(err) => {
                    self.error_at(el, &err);
                    return false;
                }
            }
        }

        if item_value.meta.flag_status != FlagStatus::Disabled {
            style.entries.push(StyleEntry {
                key,
                value: item_value,
                source,
                comment: String::new(),
            });
        }
        true
    }

    /// Port of `ResourceParser::ParseStyle` (also used for `configVarying`).
    fn parse_style(
        &mut self,
        ty: ResourceType,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
    ) -> bool {
        out.name.ty = ResourceNamedType::with_default_name(ty);

        let mut style = Style::default();

        if let Some(parent_attr) = el.find_attribute("", "parent") {
            // If the parent is empty, we don't have a parent, but we also
            // don't infer either.
            let parent_str = trim_whitespace(&parent_attr.value);
            if !parent_str.is_empty() {
                match parse_style_parent_reference(parent_str) {
                    Ok(Some(mut parent)) => {
                        // Transform the namespace prefix to the actual
                        // package name, and mark the reference as private if
                        // appropriate.
                        resolve_package(ns_stack, &mut parent);
                        style.parent = Some(parent);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let source = out.source.clone();
                        self.error(&source, &err);
                        return false;
                    }
                }
            }
        } else {
            // No parent was specified, so try inferring it from the style
            // name.
            if let Some(pos) = out.name.entry.rfind('.') {
                style.parent_inferred = true;
                style.parent = Some(Reference::from_name(ResourceName::new(
                    "",
                    ResourceType::Style,
                    &out.name.entry[..pos],
                )));
            }
        }

        let mut error = false;
        for child in el.child_elements() {
            if child.namespace_uri.is_empty() && child.name == "item" {
                let pushed = push_decls(ns_stack, child);
                if !self.parse_style_item(child, ns_stack, &mut style) {
                    error = true;
                }
                pop_decls(ns_stack, pushed);
            } else if !should_ignore_element(child) {
                self.error_at(child, &format!(":{}>", child.name));
                error = true;
            }
        }

        if error {
            return false;
        }
        out.value = Some(Value::new(ValueKind::Style(style)));
        true
    }

    /// Port of `ResourceParser::ParseDeclareStyleable`.
    fn parse_declare_styleable(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
    ) -> bool {
        out.name.ty = ResourceNamedType::with_default_name(ResourceType::Styleable);

        if !self.options.preserve_visibility_of_styleables {
            // This was added in change Idd21b5de4d20be06c6f8c8eb5a22ccd68afc4927
            // to mimic aapt1, but no one knows exactly what for.
            out.visibility_level = VisibilityLevel::Public;
        }

        // Declare-styleable only ends up in default config.
        if !out.config.is_default() {
            let source = out.source.clone();
            let config = out.config.clone();
            let entry = out.name.entry.clone();
            self.warn(
                &source,
                &format!("ignoring configuration '{config}' for styleable {entry}"),
            );
            out.config = ConfigDescription::default();
        }

        let mut styleable = Styleable::default();
        let mut error = false;
        for child in el.child_elements() {
            let item_source = self.source_at(child);
            if child.namespace_uri.is_empty() && child.name == "attr" {
                let Some(name) = find_non_empty_attr(child, "name") else {
                    self.error(&item_source, "<attr> tag must have a 'name' attribute");
                    error = true;
                    continue;
                };

                // If this is a declaration, the package name may be in the
                // name (e.g. <attr name="android:text"/>).
                let pushed = push_decls(ns_stack, child);
                let mut child_ref = parse_xml_attribute_name(name);
                resolve_package(ns_stack, &mut child_ref);

                let Some(child_name) = child_ref.name.clone() else {
                    self.error(
                        &item_source,
                        &format!("<attr> tag has invalid name '{name}'"),
                    );
                    error = true;
                    pop_decls(ns_stack, pushed);
                    continue;
                };

                // Create the ParsedResource that will add the attribute to
                // the table.
                let mut child_resource = ParsedResource {
                    name: child_name,
                    source: item_source.clone(),
                    comment: child.comment.clone(),
                    ..Default::default()
                };
                if let Some(visibility) = self.options.visibility {
                    child_resource.visibility_level = visibility;
                }

                if !self.parse_attr_impl(child, &mut child_resource, true) {
                    error = true;
                    pop_decls(ns_stack, pushed);
                    continue;
                }
                pop_decls(ns_stack, pushed);

                // NOTE: the C++ also attaches the comment/source to the
                // reference itself; our `Reference` carries no metadata, so
                // styleable-entry comments are not preserved.
                styleable.entries.push(child_ref);

                // Do not add referenced attributes that do not define a
                // format to the table.
                let declares_format = matches!(
                    &child_resource.value,
                    Some(Value { kind: ValueKind::Attribute(attr), .. })
                        if attr.type_mask != format::ANY
                );
                if declares_format {
                    out.child_resources.push(child_resource);
                }
            } else if !should_ignore_element(child) {
                self.error(
                    &item_source,
                    &format!("unknown tag <{}:{}>", child.namespace_uri, child.name),
                );
                error = true;
            }
        }

        if error {
            return false;
        }
        out.value = Some(Value::new(ValueKind::Styleable(styleable)));
        true
    }

    // ───────────────────────── arrays / plurals ────────────────────────

    /// Port of `ResourceParser::ParseArray`.
    fn parse_array(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
    ) -> bool {
        let mut resource_format = format::ANY;
        if let Some(format_attr) = find_non_empty_attr(el, "format") {
            resource_format = parse_format_type_no_enums_or_flags(format_attr);
            if resource_format == 0 {
                self.error_at(el, &format!("'{format_attr}' is an invalid format"));
                return false;
            }
        }
        self.parse_array_impl(el, ns_stack, out, resource_format)
    }

    /// Port of `ResourceParser::ParseArrayImpl` (`string-array` passes
    /// `STRING`, `integer-array` passes `INTEGER`).
    fn parse_array_impl(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
        type_mask: u32,
    ) -> bool {
        out.name.ty = ResourceNamedType::with_default_name(ResourceType::Array);

        let mut translatable = self.options.translatable;
        if let Some(translatable_attr) = find_attr(el, "translatable") {
            match parse_bool(translatable_attr) {
                Some(value) => translatable = value,
                None => {
                    let source = out.source.clone();
                    self.error(&source, "invalid value for 'translatable'. Must be a boolean");
                    return false;
                }
            }
        }

        let mut array = Array::default();
        let mut error = false;
        for child in el.child_elements() {
            let item_source = self.source_at(child);
            if child.namespace_uri.is_empty() && child.name == "item" {
                let flag = parse_flag(find_attr_ns(child, SCHEMA_ANDROID, "featureFlag"));
                let pushed = push_decls(ns_stack, child);
                let item = self.parse_xml(child, ns_stack, type_mask, false);
                pop_decls(ns_stack, pushed);
                let Some(item) = item else {
                    self.error(&item_source, "could not parse array item");
                    error = true;
                    continue;
                };
                let mut item_value = ItemValue::new(item);
                item_value.meta.flag = flag.clone();
                match get_flag_status(&flag, &self.options.feature_flags) {
                    Ok(status) => item_value.meta.flag_status = status,
                    Err(err) => {
                        self.error(&item_source, &err);
                        error = true;
                        continue;
                    }
                }
                item_value.meta.source = item_source;
                array.elements.push(item_value);
            } else if !should_ignore_element(child) {
                self.error_at(
                    child,
                    &format!("unknown tag <{}:{}>", child.namespace_uri, child.name),
                );
                error = true;
            }
        }

        if error {
            return false;
        }
        let mut value = Value::new(ValueKind::Array(array));
        value.meta.translatable = translatable;
        out.value = Some(value);
        true
    }

    /// Port of `ResourceParser::ParsePlural`.
    fn parse_plural(
        &mut self,
        el: &Element,
        ns_stack: &mut Vec<(String, String)>,
        out: &mut ParsedResource,
    ) -> bool {
        out.name.ty = ResourceNamedType::with_default_name(ResourceType::Plurals);

        let mut plural = Plural::default();
        let mut error = false;
        for child in el.child_elements() {
            let item_source = self.source_at(child);
            if child.namespace_uri.is_empty() && child.name == "item" {
                let Some(quantity) = find_non_empty_attr(child, "quantity") else {
                    self.error(
                        &item_source,
                        "<item> in <plurals> requires attribute 'quantity'",
                    );
                    error = true;
                    continue;
                };
                let trimmed_quantity = trim_whitespace(quantity);
                let index = match trimmed_quantity {
                    "zero" => PLURAL_ZERO,
                    "one" => PLURAL_ONE,
                    "two" => PLURAL_TWO,
                    "few" => PLURAL_FEW,
                    "many" => PLURAL_MANY,
                    "other" => PLURAL_OTHER,
                    _ => {
                        self.error(
                            &item_source,
                            &format!(
                                "<item> in <plural> has invalid value '{trimmed_quantity}' for \
                                 attribute 'quantity'"
                            ),
                        );
                        error = true;
                        continue;
                    }
                };

                if plural.values[index].is_some() {
                    self.error(
                        &item_source,
                        &format!("duplicate quantity '{trimmed_quantity}'"),
                    );
                    error = true;
                    continue;
                }

                let pushed = push_decls(ns_stack, child);
                let item = self.parse_xml(child, ns_stack, format::STRING, false);
                pop_decls(ns_stack, pushed);
                let Some(item) = item else {
                    error = true;
                    continue;
                };
                let mut item_value = ItemValue::new(item);
                item_value.meta.source = item_source;
                plural.values[index] = Some(item_value);
            } else if !should_ignore_element(child) {
                self.error(
                    &item_source,
                    &format!("unknown tag <{}:{}>", child.namespace_uri, child.name),
                );
                error = true;
            }
        }

        if error {
            return false;
        }
        out.value = Some(Value::new(ValueKind::Plural(plural)));
        true
    }

    // ──────────────────── XML subtree flattening ───────────────────────

    /// Builds a flat string (plus spans/untranslatable sections) from an
    /// element's body. Port of `ResourceParser::FlattenXmlSubtree` +
    /// `CreateFlattenSubTree`, walking the DOM instead of pull events.
    fn flatten_subtree(
        &mut self,
        el: &Element,
        ns_stack: &[(String, String)],
    ) -> Option<FlattenedXmlSubTree> {
        let mut ops: Vec<FlatOp> = Vec::new();
        let mut raw_value = String::new();
        let mut saw_span_node = false;
        if !self.flatten_children(el, false, &mut ops, &mut raw_value, &mut saw_span_node) {
            return None;
        }

        if !saw_span_node {
            // If there were no spans, this string is treated a little
            // differently (according to AAPT): strip leading whitespace from
            // the first segment and trailing whitespace from the last.
            let first = ops.iter().position(|op| matches!(op, FlatOp::Text(_)));
            let last = ops.iter().rposition(|op| matches!(op, FlatOp::Text(_)));
            if let Some(index) = first {
                if let FlatOp::Text(text) = &mut ops[index] {
                    let trimmed = trim_leading_whitespace(text);
                    if trimmed.len() != text.len() {
                        *text = trimmed.to_string();
                    }
                }
            }
            if let Some(index) = last {
                if let FlatOp::Text(text) = &mut ops[index] {
                    let trimmed = trim_trailing_whitespace(text);
                    if trimmed.len() != text.len() {
                        *text = trimmed.to_string();
                    }
                }
            }
        }

        // Have the flattened structure feed the StringBuilder, which takes
        // care of recording the correctly adjusted Spans and
        // UntranslatableSections.
        let mut builder = StringBuilder::new(false);
        let mut span_handles: Vec<usize> = Vec::new();
        let mut untranslatable_handles: Vec<usize> = Vec::new();
        for op in &ops {
            match op {
                FlatOp::Text(text) => {
                    builder.append_text(text);
                }
                FlatOp::StartSpan(name) => span_handles.push(builder.start_span(name)),
                FlatOp::EndSpan => {
                    if let Some(handle) = span_handles.pop() {
                        builder.end_span(handle);
                    }
                }
                FlatOp::StartUntranslatable => {
                    untranslatable_handles.push(builder.start_untranslatable());
                }
                FlatOp::EndUntranslatable => {
                    if let Some(handle) = untranslatable_handles.pop() {
                        builder.end_untranslatable(handle);
                    }
                }
            }
        }

        let processed = match builder.finish() {
            Ok(processed) => processed,
            Err(err) => {
                self.error_at(el, &err);
                return None;
            }
        };

        Some(FlattenedXmlSubTree {
            raw_value,
            style_string: StyleString { str: processed.text, spans: processed.spans },
            untranslatable_sections: processed.untranslatable_sections,
            namespace_stack: ns_stack.to_vec(),
            source: self.source_at(el),
        })
    }

    /// Recursive DOM walk for [`Self::flatten_subtree`].
    fn flatten_children(
        &mut self,
        el: &Element,
        in_untranslatable: bool,
        ops: &mut Vec<FlatOp>,
        raw_value: &mut String,
        saw_span_node: &mut bool,
    ) -> bool {
        for child in &el.children {
            match child {
                Node::Text(text) => {
                    ops.push(FlatOp::Text(text.text.clone()));
                    raw_value.push_str(&text.text);
                }
                Node::Element(child_el) => {
                    if child_el.namespace_uri.is_empty() {
                        // An HTML-like tag, encoded as a span:
                        // `tag;attr1=value1;attr2=value2;…`.
                        let mut span_name = child_el.name.clone();
                        for attr in &child_el.attributes {
                            span_name.push(';');
                            span_name.push_str(&attr.name);
                            span_name.push('=');
                            span_name.push_str(&attr.value);
                        }
                        *saw_span_node = true;
                        ops.push(FlatOp::StartSpan(span_name));
                        if !self.flatten_children(
                            child_el,
                            in_untranslatable,
                            ops,
                            raw_value,
                            saw_span_node,
                        ) {
                            return false;
                        }
                        ops.push(FlatOp::EndSpan);
                    } else if child_el.namespace_uri == XLIFF_NS_URI {
                        // An XLIFF tag, which is not encoded as a span.
                        if child_el.name == "g" {
                            // Nested <xliff:g> tags are illegal.
                            if in_untranslatable {
                                self.error_at(child_el, "illegal nested XLIFF 'g' tag");
                                return false;
                            }
                            ops.push(FlatOp::StartUntranslatable);
                            if !self.flatten_children(child_el, true, ops, raw_value, saw_span_node)
                            {
                                return false;
                            }
                            ops.push(FlatOp::EndUntranslatable);
                        } else {
                            // Ignore unknown XLIFF tags, but don't warn.
                            if !self.flatten_children(
                                child_el,
                                in_untranslatable,
                                ops,
                                raw_value,
                                saw_span_node,
                            ) {
                                return false;
                            }
                        }
                    } else {
                        // Besides XLIFF, any other namespaced tag is
                        // unsupported and ignored.
                        let source = self.source_at(child_el);
                        self.warn(
                            &source,
                            &format!(
                                "ignoring element '{}' with unknown namespace '{}'",
                                child_el.name, child_el.namespace_uri
                            ),
                        );
                        if !self.flatten_children(
                            child_el,
                            in_untranslatable,
                            ops,
                            raw_value,
                            saw_span_node,
                        ) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }
}

/// Flattening operations emitted by the DOM walk, replayed into the
/// [`StringBuilder`]. Stands in for the C++ `Node`/`SegmentNode`/
/// `SpanNode`/`UntranslatableNode` tree.
#[derive(Debug, Clone)]
enum FlatOp {
    Text(String),
    StartSpan(String),
    EndSpan,
    StartUntranslatable,
    EndUntranslatable,
}

// ────────────────────────── free helpers ───────────────────────────────

/// Returns true for `<skip>`/`<eat-comment>`, which can be safely ignored.
fn should_ignore_element(el: &Element) -> bool {
    el.namespace_uri.is_empty() && (el.name == "skip" || el.name == "eat-comment")
}

/// `xml::FindAttribute`: the attribute's value, whitespace-trimmed.
fn find_attr<'el>(el: &'el Element, name: &str) -> Option<&'el str> {
    el.find_attribute("", name).map(|attr| trim_whitespace(&attr.value))
}

/// `xml::FindAttribute` with an explicit namespace.
fn find_attr_ns<'el>(el: &'el Element, namespace_uri: &str, name: &str) -> Option<&'el str> {
    el.find_attribute(namespace_uri, name).map(|attr| trim_whitespace(&attr.value))
}

/// `xml::FindNonEmptyAttribute`: trimmed and non-empty.
fn find_non_empty_attr<'el>(el: &'el Element, name: &str) -> Option<&'el str> {
    find_attr(el, name).filter(|value| !value.is_empty())
}

/// Port of `aapt::ParseFlag` (cmd/Util.cpp): `[!]flag.name`.
pub fn parse_flag(flag_text: Option<&str>) -> Option<FeatureFlagAttribute> {
    let flag_text = flag_text?;
    if flag_text.is_empty() {
        return None;
    }
    Some(match flag_text.strip_prefix('!') {
        Some(rest) => FeatureFlagAttribute { name: rest.to_string(), negated: true },
        None => FeatureFlagAttribute { name: flag_text.to_string(), negated: false },
    })
}

/// Port of `aapt::GetFlagStatus` (cmd/Util.cpp).
pub fn get_flag_status(
    flag: &Option<FeatureFlagAttribute>,
    feature_flag_values: &HashMap<String, FeatureFlagProperties>,
) -> Result<FlagStatus, String> {
    let Some(flag) = flag else {
        return Ok(FlagStatus::NoFlag);
    };
    let Some(properties) = feature_flag_values.get(&flag.name) else {
        return Err(format!("Resource flag value undefined: {}", flag.name));
    };
    if !properties.read_only {
        return Err(format!("Only read only flags may be used with resources: {}", flag.name));
    }
    let Some(enabled) = properties.enabled else {
        return Err(format!("Only flags with a value may be used with resources: {}", flag.name));
    };
    Ok(if enabled != flag.negated { FlagStatus::Enabled } else { FlagStatus::Disabled })
}

/// Port of the C++ `ParseFormatTypeNoEnumsOrFlags`.
fn parse_format_type_no_enums_or_flags(piece: &str) -> u32 {
    match piece {
        "reference" => format::REFERENCE,
        "string" => format::STRING,
        "integer" => format::INTEGER,
        "boolean" => format::BOOLEAN,
        "color" => format::COLOR,
        "float" => format::FLOAT,
        "dimension" => format::DIMENSION,
        "fraction" => format::FRACTION,
        _ => 0,
    }
}

/// Port of the C++ `ParseFormatType`.
fn parse_format_type(piece: &str) -> u32 {
    match piece {
        "enum" => format::ENUM,
        "flags" => format::FLAGS,
        _ => parse_format_type_no_enums_or_flags(piece),
    }
}

/// Port of the C++ `ParseFormatAttribute`: `"reference|string|…"` → mask.
fn parse_format_attribute(s: &str) -> u32 {
    let mut mask = 0u32;
    for part in s.split('|') {
        let ty = parse_format_type(trim_whitespace(part));
        if ty == 0 {
            return 0;
        }
        mask |= ty;
    }
    mask
}

/// The C++ `elToItemMap`: element name → (type, implied format).
fn item_type_format(name: &str) -> Option<(ResourceType, u32)> {
    Some(match name {
        "bool" => (ResourceType::Bool, format::BOOLEAN),
        "color" => (ResourceType::Color, format::COLOR),
        "configVarying" => (ResourceType::ConfigVarying, format::ANY),
        "dimen" => (
            ResourceType::Dimen,
            format::FLOAT | format::FRACTION | format::DIMENSION,
        ),
        "drawable" => (ResourceType::Drawable, format::COLOR),
        "fraction" => (
            ResourceType::Fraction,
            format::FLOAT | format::FRACTION | format::DIMENSION,
        ),
        "integer" => (ResourceType::Integer, format::INTEGER),
        "string" => (ResourceType::String, format::STRING),
        _ => return None,
    })
}

/// The C++ `elToBagMap` keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BagKind {
    AddResource,
    Array,
    Attr,
    ConfigVarying,
    DeclareStyleable,
    IntegerArray,
    Symbol,
    Overlayable,
    Plurals,
    Public,
    PublicGroup,
    StagingPublicGroup,
    StagingPublicGroupFinal,
    StringArray,
    Style,
}

fn bag_kind(name: &str) -> Option<BagKind> {
    Some(match name {
        "add-resource" => BagKind::AddResource,
        "array" => BagKind::Array,
        "attr" => BagKind::Attr,
        "configVarying" => BagKind::ConfigVarying,
        "declare-styleable" => BagKind::DeclareStyleable,
        "integer-array" => BagKind::IntegerArray,
        "java-symbol" | "symbol" => BagKind::Symbol,
        "overlayable" => BagKind::Overlayable,
        "plurals" => BagKind::Plurals,
        "public" => BagKind::Public,
        "public-group" => BagKind::PublicGroup,
        "staging-public-group" => BagKind::StagingPublicGroup,
        "staging-public-group-final" => BagKind::StagingPublicGroupFinal,
        "string-array" => BagKind::StringArray,
        "style" => BagKind::Style,
        _ => return None,
    })
}

/// The idmap2 `kPolicyStringToFlag` table.
fn policy_from_str(s: &str) -> Option<u32> {
    Some(match s {
        "public" => policy::PUBLIC,
        "product" => policy::PRODUCT_PARTITION,
        "system" => policy::SYSTEM_PARTITION,
        "vendor" => policy::VENDOR_PARTITION,
        "signature" => policy::SIGNATURE,
        "odm" => policy::ODM_PARTITION,
        "oem" => policy::OEM_PARTITION,
        "actor" => policy::ACTOR_SIGNATURE,
        "config_signature" => policy::CONFIG_SIGNATURE,
        _ => return None,
    })
}

/// Resolves a reference's package alias against the in-scope namespace
/// declarations (innermost wins), keeping only declarations whose URI is
/// a package namespace. Port of `xml::ResolvePackage` +
/// `XmlPullParser::TransformPackageAlias`.
fn resolve_package(ns_stack: &[(String, String)], reference: &mut Reference) {
    let Some(name) = &mut reference.name else {
        return;
    };
    if name.package.is_empty() {
        // An empty alias refers to the local package; the package stays
        // empty and the reference visibility is unchanged.
        return;
    }
    for (prefix, uri) in ns_stack.iter().rev() {
        if let Some(extracted) = extract_package_from_namespace(uri) {
            if prefix == &name.package {
                name.package = extracted.package;
                // If the reference was already private (with a * prefix) and
                // the namespace is public, the reference stays private.
                reference.private_reference |= extracted.private_namespace;
                return;
            }
        }
    }
}

/// Pushes an element's namespace declarations; returns how many were added.
fn push_decls(ns_stack: &mut Vec<(String, String)>, el: &Element) -> usize {
    for decl in &el.namespace_decls {
        ns_stack.push((decl.prefix.clone(), decl.uri.clone()));
    }
    el.namespace_decls.len()
}

fn pop_decls(ns_stack: &mut Vec<(String, String)>, count: usize) {
    for _ in 0..count {
        ns_stack.pop();
    }
}

/// ASCII-whitespace variants of `util::TrimLeadingWhitespace` /
/// `util::TrimTrailingWhitespace`.
fn trim_leading_whitespace(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_ascii_whitespace())
}

fn trim_trailing_whitespace(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_ascii_whitespace())
}

/// Checks that a formatted string is safe for translation: multiple
/// substitutions must be positional (`%1$s`), and strings destined for
/// `Time.format()` are exempted. Port of `util::VerifyJavaStringFormat`.
pub fn verify_java_string_format(s: &str) -> bool {
    let bytes = s.as_bytes();
    let end = bytes.len();
    let mut c = 0usize;

    let mut arg_count = 0usize;
    let mut nonpositional = false;
    while c < end {
        if bytes[c] == b'%' && c + 1 < end {
            c += 1;

            if bytes[c] == b'%' || bytes[c] == b'n' {
                c += 1;
                continue;
            }

            arg_count += 1;

            let num_digits = {
                let mut n = 0usize;
                while c + n < end && bytes[c + n].is_ascii_digit() {
                    n += 1;
                }
                n
            };
            if num_digits > 0 {
                c += num_digits;
                if c < end && bytes[c] != b'$' {
                    // The digits were a size, but not a positional argument.
                    nonpositional = true;
                }
            } else if bytes[c] == b'<' {
                // Reusing the last argument is a bad idea since positions can
                // be moved around during translation.
                nonpositional = true;
                c += 1;
                // Optionally we can have a $ after.
                if c < end && bytes[c] == b'$' {
                    c += 1;
                }
            } else {
                nonpositional = true;
            }

            // Ignore size, width, flags, etc.
            while c < end
                && matches!(bytes[c], b'-' | b'#' | b'+' | b' ' | b',' | b'(' | b'0'..=b'9')
            {
                c += 1;
            }

            // Shortcut to detect strings that are going to Time.format()
            // instead of String.format(): DFKMWZkmwyz only appear in the
            // former.
            if c < end {
                match bytes[c] {
                    b'D' | b'F' | b'K' | b'M' | b'W' | b'Z' | b'k' | b'm' | b'w' | b'y' | b'z' => {
                        return true;
                    }
                    _ => {}
                }
            }
        }

        if c < end {
            c += 1;
        }
    }

    // Multiple arguments were specified, but some or all were non-positional.
    // Translated strings may rearrange the order of the arguments, which
    // will break the string.
    !(arg_count > 1 && nonpositional)
}

// ─────────────────────────────── tests ─────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::res::table::ResourceEntry;
    use crate::res::utils::{parse_resource_name, try_parse_flag_symbol};
    use crate::res::value::{res_value_type, ReferenceType, DATA_NULL_EMPTY};
    use crate::xml::parse_source_xml;

    const XML_PREAMBLE: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n";

    fn parse_with(
        table: &mut ResourceTable,
        fragment: &str,
        config: ConfigDescription,
        options: ResourceParserOptions,
    ) -> Result<(), Vec<String>> {
        let xml = format!("{XML_PREAMBLE}<resources>\n{fragment}\n</resources>");
        let doc = parse_source_xml("test", &xml).map_err(|e| vec![e.to_string()])?;
        let mut parser = ResourceParser::new(table, Source::new("test"), config, options);
        parser.parse(&doc)
    }

    /// Mirrors `ResourceParserTest::TestParse`.
    fn test_parse(fragment: &str) -> Result<ResourceTable, Vec<String>> {
        let mut table = ResourceTable::new();
        parse_with(
            &mut table,
            fragment,
            ConfigDescription::default(),
            ResourceParserOptions::default(),
        )?;
        Ok(table)
    }

    fn name(s: &str) -> ResourceName {
        parse_resource_name(s).expect("valid resource name for test").0
    }

    fn get_entry<'t>(table: &'t ResourceTable, name_str: &str) -> Option<&'t ResourceEntry> {
        table.find_resource(&name(name_str)).map(|r| r.entry)
    }

    fn get_value_for_config_product<'t>(
        table: &'t ResourceTable,
        name_str: &str,
        config: &ConfigDescription,
        product: &str,
    ) -> Option<&'t Value> {
        get_entry(table, name_str)?.find_value(config, product)?.value.as_ref()
    }

    fn get_value<'t>(table: &'t ResourceTable, name_str: &str) -> Option<&'t Value> {
        get_value_for_config_product(table, name_str, &ConfigDescription::default(), "")
    }

    fn get_string<'t>(table: &'t ResourceTable, name_str: &str) -> &'t str {
        match &get_value(table, name_str).expect("expected a value").kind {
            ValueKind::Item(Item::String { value, .. }) => value,
            other => panic!("expected a String for {name_str}, got {other:?}"),
        }
    }

    fn get_attr<'t>(table: &'t ResourceTable, name_str: &str) -> Option<&'t Attribute> {
        match &get_value(table, name_str)?.kind {
            ValueKind::Attribute(attr) => Some(attr),
            _ => None,
        }
    }

    fn get_style<'t>(table: &'t ResourceTable, name_str: &str) -> &'t Style {
        match &get_value(table, name_str).expect("expected a style").kind {
            ValueKind::Style(style) => style,
            other => panic!("expected a Style for {name_str}, got {other:?}"),
        }
    }

    #[test]
    fn fail_to_parse_with_no_root_resources_element() {
        let doc = parse_source_xml("test", "<attr name=\"foo\"/>").unwrap();
        let mut table = ResourceTable::new();
        let mut parser = ResourceParser::new(
            &mut table,
            Source::new("test"),
            ConfigDescription::default(),
            ResourceParserOptions::default(),
        );
        let err = parser.parse(&doc).unwrap_err();
        assert!(err[0].contains("root element must be <resources>"), "{err:?}");
    }

    #[test]
    fn parse_quoted_string() {
        let table = test_parse(r#"<string name="foo">   "  hey there " </string>"#).unwrap();
        match &get_value(&table, "string/foo").unwrap().kind {
            ValueKind::Item(Item::String { value, untranslatable_sections }) => {
                assert_eq!(value, "  hey there ");
                assert!(untranslatable_sections.is_empty());
            }
            other => panic!("{other:?}"),
        }

        let table = test_parse(r"<string name='bar'>Isn\'t it cool?</string>").unwrap();
        assert_eq!(get_string(&table, "string/bar"), "Isn't it cool?");

        let table = test_parse(r#"<string name="baz">"Isn't it cool?"</string>"#).unwrap();
        assert_eq!(get_string(&table, "string/baz"), "Isn't it cool?");
    }

    #[test]
    fn parse_escaped_string() {
        let table = test_parse(r#"<string name="foo">\?123</string>"#).unwrap();
        assert_eq!(get_string(&table, "string/foo"), "?123");

        let table =
            test_parse("<string name=\"bar\">This isn\\\u{2019}t a bad string</string>").unwrap();
        assert_eq!(get_string(&table, "string/bar"), "This isn\u{2019}t a bad string");
    }

    #[test]
    fn parse_formatted_string() {
        assert!(test_parse(r#"<string name="foo">%d %s</string>"#).is_err());
        assert!(test_parse(r#"<string name="foo">%1$d %2$s</string>"#).is_ok());
    }

    #[test]
    fn parse_platform_independent_newline() {
        assert!(test_parse(r#"<string name="foo">%1$s %n %2$s</string>"#).is_ok());
    }

    #[test]
    fn parse_styled_string() {
        // A non-ASCII code point verifies that span indices use UTF-16
        // lengths, not UTF-8 lengths.
        let input = "<string name=\"foo\">This is my aunt\u{2019}s \
                     <b>fickle <small>string</small></b></string>";
        let table = test_parse(input).unwrap();

        match &get_value(&table, "string/foo").unwrap().kind {
            ValueKind::Item(Item::StyledString { value, spans, untranslatable_sections }) => {
                assert_eq!(value, "This is my aunt\u{2019}s fickle string");
                assert_eq!(spans.len(), 2);
                assert!(untranslatable_sections.is_empty());

                assert_eq!(spans[0].name, "b");
                assert_eq!(spans[0].first_char, 18);
                assert_eq!(spans[0].last_char, 30);

                assert_eq!(spans[1].name, "small");
                assert_eq!(spans[1].first_char, 25);
                assert_eq!(spans[1].last_char, 30);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_string_with_whitespace() {
        let table = test_parse(r#"<string name="foo">  This is what  I think  </string>"#).unwrap();
        assert_eq!(get_string(&table, "string/foo"), "This is what I think");

        let table =
            test_parse(r#"<string name="foo2">"  This is what  I think  "</string>"#).unwrap();
        assert_eq!(get_string(&table, "string/foo2"), "  This is what  I think  ");
    }

    #[test]
    fn parse_string_truncate_ascii() {
        // Truncates leading and trailing ASCII whitespace.
        let table = test_parse(r#"<string name="foo">&#32;Hello&#32;</string>"#).unwrap();
        assert_eq!(get_string(&table, "string/foo"), "Hello");

        // AAPT does not truncate unicode-escaped whitespace: the escapes
        // are still raw text when leading/trailing trimming happens.
        let table = test_parse(r#"<string name="foo2">\u0020\Hello\u0020</string>"#).unwrap();
        assert_eq!(get_string(&table, "string/foo2"), " Hello ");

        // Non-ASCII whitespace is preserved.
        let table =
            test_parse(r#"<string name="foo3">&#160;Hello&#x202F;World&#160;</string>"#).unwrap();
        assert_eq!(get_string(&table, "string/foo3"), "\u{A0}Hello\u{202F}World\u{A0}");

        let table = test_parse(r#"<string name="foo4">2005年6月1日</string>"#).unwrap();
        assert_eq!(get_string(&table, "string/foo4"), "2005年6月1日");
    }

    #[test]
    fn parse_styled_string_with_whitespace() {
        let input = r#"<string name="foo">  <b> My <i> favorite</i> string </b>  </string>"#;
        let table = test_parse(input).unwrap();

        match &get_value(&table, "string/foo").unwrap().kind {
            ValueKind::Item(Item::StyledString { value, spans, untranslatable_sections }) => {
                assert_eq!(value, "  My  favorite string  ");
                assert!(untranslatable_sections.is_empty());

                assert_eq!(spans.len(), 2);
                assert_eq!(spans[0].name, "b");
                assert_eq!(spans[0].first_char, 1);
                assert_eq!(spans[0].last_char, 21);

                assert_eq!(spans[1].name, "i");
                assert_eq!(spans[1].first_char, 5);
                assert_eq!(spans[1].last_char, 13);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_string_translatable_attribute() {
        // No attribute: default is translatable.
        let table = test_parse(r#"<string name="foo1">Translate</string>"#).unwrap();
        assert!(get_value(&table, "string/foo1").unwrap().meta.translatable);

        let table =
            test_parse(r#"<string name="foo2" translatable="true">Translate</string>"#).unwrap();
        assert!(get_value(&table, "string/foo2").unwrap().meta.translatable);

        let table = test_parse(r#"<string name="foo3" translatable="false">No</string>"#).unwrap();
        assert!(!get_value(&table, "string/foo3").unwrap().meta.translatable);

        // Invalid value: must be a boolean.
        assert!(test_parse(r#"<string name="foo4" translatable="yes">Translate</string>"#).is_err());
    }

    #[test]
    fn ignore_xliff_tags_other_than_g() {
        let input = r#"
      <string name="foo" xmlns:xliff="urn:oasis:names:tc:xliff:document:1.2">
          There are <xliff:source>no</xliff:source> apples</string>"#;
        let table = test_parse(input).unwrap();
        match &get_value(&table, "string/foo").unwrap().kind {
            ValueKind::Item(Item::String { value, untranslatable_sections }) => {
                assert_eq!(value, "There are no apples");
                assert!(untranslatable_sections.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nested_xliff_g_tags_are_illegal() {
        let input = r#"
      <string name="foo" xmlns:xliff="urn:oasis:names:tc:xliff:document:1.2">
          Do not <xliff:g>translate <xliff:g>this</xliff:g></xliff:g></string>"#;
        let err = test_parse(input).unwrap_err();
        assert!(err.iter().any(|e| e.contains("illegal nested XLIFF 'g' tag")), "{err:?}");
    }

    #[test]
    fn record_untranslatable_xliff_sections_in_string() {
        let input = r#"
      <string name="foo" xmlns:xliff="urn:oasis:names:tc:xliff:document:1.2">
          There are <xliff:g id="count">%1$d</xliff:g> apples</string>"#;
        let table = test_parse(input).unwrap();
        match &get_value(&table, "string/foo").unwrap().kind {
            ValueKind::Item(Item::String { value, untranslatable_sections }) => {
                assert_eq!(value, "There are %1$d apples");
                assert_eq!(untranslatable_sections.len(), 1);
                assert_eq!(untranslatable_sections[0].start, 10);
                assert_eq!(untranslatable_sections[0].end, 14);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn record_untranslatable_xliff_sections_in_styled_string() {
        let input = r#"
      <string name="foo" xmlns:xliff="urn:oasis:names:tc:xliff:document:1.2">
          There are <b><xliff:g id="count">%1$d</xliff:g></b> apples</string>"#;
        let table = test_parse(input).unwrap();
        match &get_value(&table, "string/foo").unwrap().kind {
            ValueKind::Item(Item::StyledString { value, spans, untranslatable_sections }) => {
                assert_eq!(value, " There are %1$d apples");
                assert_eq!(untranslatable_sections.len(), 1);
                assert_eq!(untranslatable_sections[0].start, 11);
                assert_eq!(untranslatable_sections[0].end, 15);
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].name, "b");
                assert_eq!(spans[0].first_char, 11);
                assert_eq!(spans[0].last_char, 14);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_null() {
        let table = test_parse(r#"<integer name="foo">@null</integer>"#).unwrap();
        // @null must be encoded as TYPE_REFERENCE with data 0, since the
        // runtime treats TYPE_NULL as a non-existing value.
        match &get_value(&table, "integer/foo").unwrap().kind {
            ValueKind::Item(Item::Reference(reference)) => {
                assert!(reference.name.is_none());
                assert!(reference.id.is_none());
                assert_eq!(reference.reference_type, ReferenceType::Resource);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_empty() {
        let table = test_parse(r#"<integer name="foo">@empty</integer>"#).unwrap();
        match &get_value(&table, "integer/foo").unwrap().kind {
            ValueKind::Item(Item::BinaryPrimitive(value)) => {
                assert_eq!(value.data_type, res_value_type::TYPE_NULL);
                assert_eq!(value.data, DATA_NULL_EMPTY);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_attr() {
        let input = r#"
      <attr name="foo" format="string"/>
      <attr name="bar"/>"#;
        let table = test_parse(input).unwrap();

        let attr = get_attr(&table, "attr/foo").unwrap();
        assert_eq!(attr.type_mask, format::STRING);

        let attr = get_attr(&table, "attr/bar").unwrap();
        assert_eq!(attr.type_mask, format::ANY);
    }

    #[test]
    fn parse_attr_with_min_max() {
        let table =
            test_parse(r#"<attr name="foo" min="10" max="23" format="integer"/>"#).unwrap();
        let attr = get_attr(&table, "attr/foo").unwrap();
        assert_eq!(attr.type_mask, format::INTEGER);
        assert_eq!(attr.min_int, 10);
        assert_eq!(attr.max_int, 23);
    }

    #[test]
    fn fail_parse_attr_with_min_max_but_not_integer() {
        assert!(test_parse(r#"<attr name="foo" min="10" max="23" format="string"/>"#).is_err());
    }

    #[test]
    fn parse_use_and_decl_of_attr() {
        let input = r#"
      <declare-styleable name="Styleable">
        <attr name="foo" />
      </declare-styleable>
      <attr name="foo" format="string"/>"#;
        let table = test_parse(input).unwrap();
        let attr = get_attr(&table, "attr/foo").unwrap();
        assert_eq!(attr.type_mask, format::STRING);
    }

    #[test]
    fn parse_double_use_of_attr() {
        let input = r#"
      <declare-styleable name="Theme">
        <attr name="foo" />
      </declare-styleable>
      <declare-styleable name="Window">
        <attr name="foo" format="boolean"/>
      </declare-styleable>"#;
        let table = test_parse(input).unwrap();
        let attr = get_attr(&table, "attr/foo").unwrap();
        assert_eq!(attr.type_mask, format::BOOLEAN);
    }

    #[test]
    fn parse_enum_attr() {
        let input = r#"
      <attr name="foo">
        <enum name="bar" value="0"/>
        <enum name="bat" value="0x1"/>
        <enum name="baz" value="2"/>
      </attr>"#;
        let table = test_parse(input).unwrap();

        let attr = get_attr(&table, "attr/foo").unwrap();
        assert_eq!(attr.type_mask, format::ENUM);
        assert_eq!(attr.symbols.len(), 3);

        assert_eq!(attr.symbols[0].symbol.name.as_ref().unwrap().entry, "bar");
        assert_eq!(attr.symbols[0].value, 0);
        assert_eq!(attr.symbols[0].data_type, res_value_type::TYPE_INT_DEC);

        assert_eq!(attr.symbols[1].symbol.name.as_ref().unwrap().entry, "bat");
        assert_eq!(attr.symbols[1].value, 1);
        assert_eq!(attr.symbols[1].data_type, res_value_type::TYPE_INT_HEX);

        assert_eq!(attr.symbols[2].symbol.name.as_ref().unwrap().entry, "baz");
        assert_eq!(attr.symbols[2].value, 2);
        assert_eq!(attr.symbols[2].data_type, res_value_type::TYPE_INT_DEC);
    }

    #[test]
    fn parse_flag_attr() {
        let input = r#"
      <attr name="foo">
        <flag name="bar" value="0"/>
        <flag name="bat" value="1"/>
        <flag name="baz" value="2"/>
      </attr>"#;
        let table = test_parse(input).unwrap();

        let attr = get_attr(&table, "attr/foo").unwrap();
        assert_eq!(attr.type_mask, format::FLAGS);
        assert_eq!(attr.symbols.len(), 3);
        assert_eq!(attr.symbols[0].symbol.name.as_ref().unwrap().entry, "bar");
        assert_eq!(attr.symbols[0].value, 0);
        assert_eq!(attr.symbols[1].symbol.name.as_ref().unwrap().entry, "bat");
        assert_eq!(attr.symbols[1].value, 1);
        assert_eq!(attr.symbols[2].symbol.name.as_ref().unwrap().entry, "baz");
        assert_eq!(attr.symbols[2].value, 2);

        match try_parse_flag_symbol(attr, "baz|bat") {
            Some(Item::BinaryPrimitive(value)) => assert_eq!(value.data, 1 | 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fail_to_parse_enum_attr_with_non_unique_keys() {
        let input = r#"
      <attr name="foo">
        <enum name="bar" value="0"/>
        <enum name="bat" value="1"/>
        <enum name="bat" value="2"/>
      </attr>"#;
        let err = test_parse(input).unwrap_err();
        assert!(err.iter().any(|e| e.contains("duplicate symbol 'bat'")), "{err:?}");
    }

    #[test]
    fn parse_style() {
        let input = r#"
      <style name="foo" parent="@style/fu">
        <item name="bar">#ffffffff</item>
        <item name="bat">@string/hey</item>
        <item name="baz"><b>hey</b></item>
      </style>"#;
        let table = test_parse(input).unwrap();

        let style = get_style(&table, "style/foo");
        assert_eq!(style.parent.as_ref().unwrap().name, Some(name("style/fu")));
        assert_eq!(style.entries.len(), 3);
        assert_eq!(style.entries[0].key.name, Some(name("attr/bar")));
        assert_eq!(style.entries[1].key.name, Some(name("attr/bat")));
        assert_eq!(style.entries[2].key.name, Some(name("attr/baz")));
    }

    #[test]
    fn parse_style_with_shorthand_parent() {
        let table = test_parse(r#"<style name="foo" parent="com.app:Theme"/>"#).unwrap();
        let style = get_style(&table, "style/foo");
        assert_eq!(style.parent.as_ref().unwrap().name, Some(name("com.app:style/Theme")));
    }

    #[test]
    fn parse_style_with_package_aliased_parent() {
        let input = r#"
      <style xmlns:app="http://schemas.android.com/apk/res/android"
          name="foo" parent="app:Theme"/>"#;
        let table = test_parse(input).unwrap();
        let style = get_style(&table, "style/foo");
        assert_eq!(style.parent.as_ref().unwrap().name, Some(name("android:style/Theme")));
    }

    #[test]
    fn parse_style_with_package_aliased_items() {
        let input = r#"
      <style xmlns:app="http://schemas.android.com/apk/res/android" name="foo">
        <item name="app:bar">0</item>
      </style>"#;
        let table = test_parse(input).unwrap();
        let style = get_style(&table, "style/foo");
        assert_eq!(style.entries.len(), 1);
        assert_eq!(style.entries[0].key.name, Some(name("android:attr/bar")));
    }

    #[test]
    fn parse_style_with_raw_string_item() {
        let input = r#"
      <style name="foo">
        <item name="bar">
          com.helloworld.AppClass
        </item>
      </style>"#;
        let table = test_parse(input).unwrap();
        let style = get_style(&table, "style/foo");
        match &style.entries[0].value.item {
            Item::RawString(raw) => assert_eq!(raw, "com.helloworld.AppClass"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_style_with_inferred_parent() {
        let table = test_parse(r#"<style name="foo.bar"/>"#).unwrap();
        let style = get_style(&table, "style/foo.bar");
        assert_eq!(style.parent.as_ref().unwrap().name, Some(name("style/foo")));
        assert!(style.parent_inferred);
    }

    #[test]
    fn parse_style_with_inferred_parent_overridden_by_empty_parent_attribute() {
        let table = test_parse(r#"<style name="foo.bar" parent=""/>"#).unwrap();
        let style = get_style(&table, "style/foo.bar");
        assert!(style.parent.is_none());
        assert!(!style.parent_inferred);
    }

    #[test]
    fn parse_style_with_private_parent_reference() {
        let table = test_parse(r#"<style name="foo" parent="*android:style/bar" />"#).unwrap();
        let style = get_style(&table, "style/foo");
        assert!(style.parent.as_ref().unwrap().private_reference);
    }

    #[test]
    fn parse_auto_generated_id_reference() {
        let table = test_parse(r#"<string name="foo">@+id/bar</string>"#).unwrap();
        assert!(matches!(
            &get_value(&table, "id/bar").unwrap().kind,
            ValueKind::Item(Item::Id)
        ));
    }

    #[test]
    fn parse_attributes_declare_styleable() {
        let input = r#"
      <declare-styleable name="foo">
        <attr name="bar" />
        <attr name="bat" format="string|reference"/>
        <attr name="baz">
          <enum name="foo" value="1"/>
        </attr>
      </declare-styleable>"#;
        let table = test_parse(input).unwrap();

        let entry = get_entry(&table, "styleable/foo").unwrap();
        assert_eq!(entry.visibility.level, VisibilityLevel::Public);

        // attr/bar has no format, so it is not added to the table.
        assert!(get_value(&table, "attr/bar").is_none());

        let attr = get_attr(&table, "attr/bat").unwrap();
        assert_eq!(attr.type_mask, format::STRING | format::REFERENCE);
        assert!(get_value(&table, "attr/bat").unwrap().meta.weak);

        let attr = get_attr(&table, "attr/baz").unwrap();
        assert_eq!(attr.symbols.len(), 1);
        assert!(get_value(&table, "attr/baz").unwrap().meta.weak);

        assert!(matches!(
            &get_value(&table, "id/foo").unwrap().kind,
            ValueKind::Item(Item::Id)
        ));

        match &get_value(&table, "styleable/foo").unwrap().kind {
            ValueKind::Styleable(styleable) => {
                assert_eq!(styleable.entries.len(), 3);
                assert_eq!(styleable.entries[0].name, Some(name("attr/bar")));
                assert_eq!(styleable.entries[1].name, Some(name("attr/bat")));
                assert_eq!(styleable.entries[2].name, Some(name("attr/baz")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_declare_styleable_preserving_visibility() {
        let input = r#"
        <declare-styleable name="foo">
          <attr name="myattr" />
        </declare-styleable>
        <declare-styleable name="bar">
          <attr name="myattr" />
        </declare-styleable>
        <public type="styleable" name="bar" />"#;
        let mut table = ResourceTable::new();
        parse_with(
            &mut table,
            input,
            ConfigDescription::default(),
            ResourceParserOptions {
                preserve_visibility_of_styleables: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            get_entry(&table, "styleable/foo").unwrap().visibility.level,
            VisibilityLevel::Undefined
        );
        assert_eq!(
            get_entry(&table, "styleable/bar").unwrap().visibility.level,
            VisibilityLevel::Public
        );
    }

    #[test]
    fn parse_private_attributes_declare_styleable() {
        let input = r#"
      <declare-styleable xmlns:privAndroid="http://schemas.android.com/apk/prv/res/android"
          name="foo">
        <attr name="*android:bar" />
        <attr name="privAndroid:bat" />
      </declare-styleable>"#;
        let table = test_parse(input).unwrap();
        match &get_value(&table, "styleable/foo").unwrap().kind {
            ValueKind::Styleable(styleable) => {
                assert_eq!(styleable.entries.len(), 2);
                assert!(styleable.entries[0].private_reference);
                assert_eq!(styleable.entries[0].name.as_ref().unwrap().package, "android");
                assert!(styleable.entries[1].private_reference);
                assert_eq!(styleable.entries[1].name.as_ref().unwrap().package, "android");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_array() {
        let input = r#"
      <array name="foo">
        <item>@string/ref</item>
        <item>hey</item>
        <item>23</item>
      </array>"#;
        let table = test_parse(input).unwrap();
        match &get_value(&table, "array/foo").unwrap().kind {
            ValueKind::Array(array) => {
                assert_eq!(array.elements.len(), 3);
                assert!(matches!(array.elements[0].item, Item::Reference(_)));
                assert!(matches!(array.elements[1].item, Item::String { .. }));
                assert!(matches!(array.elements[2].item, Item::BinaryPrimitive(_)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_string_array() {
        let input = "
      <string-array name=\"foo\">
        <item>\"Werk\"</item>
      </string-array>";
        let table = test_parse(input).unwrap();
        assert!(matches!(
            &get_value(&table, "array/foo").unwrap().kind,
            ValueKind::Array(_)
        ));
    }

    #[test]
    fn parse_array_with_format() {
        let input = r#"
      <array name="foo" format="string">
        <item>100</item>
      </array>"#;
        let table = test_parse(input).unwrap();
        match &get_value(&table, "array/foo").unwrap().kind {
            ValueKind::Array(array) => {
                assert_eq!(array.elements.len(), 1);
                match &array.elements[0].item {
                    Item::String { value, .. } => assert_eq!(value, "100"),
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_array_with_bad_format() {
        let input = r#"
      <array name="foo" format="integer">
        <item>Hi</item>
      </array>"#;
        assert!(test_parse(input).is_err());
    }

    #[test]
    fn parse_plural() {
        let input = r#"
      <plurals name="foo">
        <item quantity="other">apples</item>
        <item quantity="one">apple</item>
      </plurals>"#;
        let table = test_parse(input).unwrap();
        match &get_value(&table, "plurals/foo").unwrap().kind {
            ValueKind::Plural(plural) => {
                assert!(plural.values[PLURAL_ZERO].is_none());
                assert!(plural.values[PLURAL_TWO].is_none());
                assert!(plural.values[PLURAL_FEW].is_none());
                assert!(plural.values[PLURAL_MANY].is_none());
                assert!(plural.values[PLURAL_ONE].is_some());
                assert!(plural.values[PLURAL_OTHER].is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_plural_duplicate_quantity_is_error() {
        let input = r#"
      <plurals name="foo">
        <item quantity="other">apples</item>
        <item quantity="other">apple</item>
      </plurals>"#;
        let err = test_parse(input).unwrap_err();
        assert!(err.iter().any(|e| e.contains("duplicate quantity 'other'")), "{err:?}");
    }

    #[test]
    fn parse_comments_with_resource() {
        let input = r#"
      <!--This is a comment-->
      <string name="foo">Hi</string>"#;
        let table = test_parse(input).unwrap();
        let value = get_value(&table, "string/foo").unwrap();
        assert_eq!(value.meta.comment, "This is a comment");
    }

    #[test]
    fn do_not_combine_multiple_comments() {
        let input = r#"
      <!--One-->
      <!--Two-->
      <string name="foo">Hi</string>"#;
        let table = test_parse(input).unwrap();
        let value = get_value(&table, "string/foo").unwrap();
        assert_eq!(value.meta.comment, "Two");
    }

    #[test]
    fn ignore_comment_before_end_tag() {
        let input = r#"
      <!--One-->
      <string name="foo">
        Hi
      <!--Two-->
      </string>"#;
        let table = test_parse(input).unwrap();
        let value = get_value(&table, "string/foo").unwrap();
        assert_eq!(value.meta.comment, "One");
    }

    #[test]
    fn parse_nested_comments() {
        // Comments from enum/flag symbols end up in R.java.
        let input = r#"
      <attr name="foo">
        <!-- The very first -->
        <enum name="one" value="1" />
      </attr>"#;
        let table = test_parse(input).unwrap();
        let attr = get_attr(&table, "attr/foo").unwrap();
        assert_eq!(attr.symbols.len(), 1);
        assert_eq!(attr.symbols[0].comment, "The very first");
    }

    // Declaring an ID as public should not require a separate definition
    // (as an ID has no value).
    #[test]
    fn parse_public_id_as_definition() {
        let table = test_parse(r#"<public type="id" name="foo"/>"#).unwrap();
        assert!(matches!(
            &get_value(&table, "id/foo").unwrap().kind,
            ValueKind::Item(Item::Id)
        ));
    }

    #[test]
    fn keep_all_products() {
        let input = r#"
      <string name="foo" product="phone">hi</string>
      <string name="foo" product="no-sdcard">ho</string>
      <string name="bar" product="">wee</string>
      <string name="baz">woo</string>
      <string name="bit" product="phablet">hoot</string>
      <string name="bot" product="default">yes</string>"#;
        let table = test_parse(input).unwrap();
        let config = ConfigDescription::default();
        assert!(get_value_for_config_product(&table, "string/foo", &config, "phone").is_some());
        assert!(get_value_for_config_product(&table, "string/foo", &config, "no-sdcard").is_some());
        assert!(get_value_for_config_product(&table, "string/bar", &config, "").is_some());
        assert!(get_value_for_config_product(&table, "string/baz", &config, "").is_some());
        assert!(get_value_for_config_product(&table, "string/bit", &config, "phablet").is_some());
        assert!(get_value_for_config_product(&table, "string/bot", &config, "default").is_some());
    }

    #[test]
    fn auto_increment_ids_in_public_group() {
        let input = r#"
      <public-group type="attr" first-id="0x01010040">
        <public name="foo" />
        <public name="bar" />
      </public-group>"#;
        let table = test_parse(input).unwrap();

        let entry = get_entry(&table, "attr/foo").unwrap();
        assert_eq!(entry.id, Some(ResourceId(0x01010040)));
        assert_eq!(entry.visibility.level, VisibilityLevel::Public);

        let entry = get_entry(&table, "attr/bar").unwrap();
        assert_eq!(entry.id, Some(ResourceId(0x01010041)));
        assert_eq!(entry.visibility.level, VisibilityLevel::Public);
    }

    #[test]
    fn staging_public_group() {
        let input = r#"
      <staging-public-group type="attr" first-id="0x01ff0049">
        <public name="foo" />
        <public name="bar" />
      </staging-public-group>"#;
        let table = test_parse(input).unwrap();

        let entry = get_entry(&table, "attr/foo").unwrap();
        assert_eq!(entry.id, Some(ResourceId(0x01ff0049)));
        assert_eq!(entry.visibility.level, VisibilityLevel::Public);
        assert!(entry.visibility.staged_api);

        let entry = get_entry(&table, "attr/bar").unwrap();
        assert_eq!(entry.id, Some(ResourceId(0x01ff004a)));
        assert_eq!(entry.visibility.level, VisibilityLevel::Public);
        assert!(entry.visibility.staged_api);
    }

    #[test]
    fn staging_public_group_final() {
        let input = r#"
      <staging-public-group-final type="attr" first-id="0x01ff0049">
        <public name="foo" />
      </staging-public-group-final>"#;
        let table = test_parse(input).unwrap();
        let entry = get_entry(&table, "attr/foo").unwrap();
        assert_eq!(entry.staged_id.map(|s| s.id), Some(ResourceId(0x01ff0049)));
    }

    #[test]
    fn strongest_symbol_visibility_wins() {
        let input = r#"
      <!-- private -->
      <java-symbol type="string" name="foo" />
      <!-- public -->
      <public type="string" name="foo" id="0x01020000" />
      <!-- private2 -->
      <java-symbol type="string" name="foo" />"#;
        let table = test_parse(input).unwrap();
        let entry = get_entry(&table, "string/foo").unwrap();
        assert_eq!(entry.visibility.level, VisibilityLevel::Public);
        assert_eq!(entry.visibility.comment, "public");
    }

    #[test]
    fn external_types_should_only_be_references() {
        assert!(test_parse(r#"<item type="layout" name="foo">@layout/bar</item>"#).is_ok());
        assert!(test_parse(r#"<item type="layout" name="bar">"this is a string"</item>"#).is_err());
    }

    #[test]
    fn add_resources_element_should_add_entry_with_undefined_symbol() {
        let table = test_parse(r#"<add-resource name="bar" type="string" />"#).unwrap();
        let entry = get_entry(&table, "string/bar").unwrap();
        assert_eq!(entry.visibility.level, VisibilityLevel::Undefined);
        assert!(entry.allow_new.is_some());
    }

    #[test]
    fn parse_item_element_with_format() {
        let table =
            test_parse(r#"<item name="foo" type="integer" format="float">0.3</item>"#).unwrap();
        match &get_value(&table, "integer/foo").unwrap().kind {
            ValueKind::Item(Item::BinaryPrimitive(value)) => {
                assert_eq!(value.data_type, res_value_type::TYPE_FLOAT);
            }
            other => panic!("{other:?}"),
        }

        assert!(
            test_parse(r#"<item name="bar" type="integer" format="fraction">100</item>"#).is_err()
        );
    }

    // An <item> without a format specifier accepts all types of values.
    #[test]
    fn parse_item_element_without_format() {
        let table = test_parse(r#"<item name="foo" type="integer">100%p</item>"#).unwrap();
        match &get_value(&table, "integer/foo").unwrap().kind {
            ValueKind::Item(Item::BinaryPrimitive(value)) => {
                assert_eq!(value.data_type, res_value_type::TYPE_FRACTION);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_config_varying_item() {
        let table = test_parse(r#"<item name="foo" type="configVarying">Hey</item>"#).unwrap();
        assert!(matches!(
            &get_value(&table, "configVarying/foo").unwrap().kind,
            ValueKind::Item(Item::String { .. })
        ));
    }

    #[test]
    fn parse_bag_element() {
        let input = r#"
      <bag name="bag" type="configVarying">
        <item name="test">Hello!</item>
      </bag>"#;
        let table = test_parse(input).unwrap();
        let style = get_style(&table, "configVarying/bag");
        assert_eq!(style.entries.len(), 1);
        assert_eq!(style.entries[0].key.name, Some(name("attr/test")));
        match &style.entries[0].value.item {
            Item::RawString(raw) => assert_eq!(raw, "Hello!"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_element_with_no_value() {
        let input = r#"
      <item type="drawable" format="reference" name="foo" />
      <string name="foo" />"#;
        let table = test_parse(input).unwrap();

        // An empty reference-format item is encoded as @null.
        match &get_value(&table, "drawable/foo").unwrap().kind {
            ValueKind::Item(Item::Reference(reference)) => {
                assert!(reference.name.is_none());
                assert!(reference.id.is_none());
            }
            other => panic!("{other:?}"),
        }

        assert_eq!(get_string(&table, "string/foo"), "");
    }

    #[test]
    fn parse_overlayable() {
        let input = r#"
      <overlayable name="Name" actor="overlay://theme">
          <policy type="signature">
            <item type="string" name="foo" />
            <item type="drawable" name="bar" />
          </policy>
      </overlayable>"#;
        let table = test_parse(input).unwrap();

        let entry = get_entry(&table, "string/foo").unwrap();
        let overlayable_item = entry.overlayable_item.as_ref().unwrap();
        let overlayable = &table.overlayables[overlayable_item.overlayable_index];
        assert_eq!(overlayable.name, "Name");
        assert_eq!(overlayable.actor, "overlay://theme");
        assert_eq!(overlayable_item.policies, policy::SIGNATURE);

        let entry = get_entry(&table, "drawable/bar").unwrap();
        let overlayable_item = entry.overlayable_item.as_ref().unwrap();
        let overlayable = &table.overlayables[overlayable_item.overlayable_index];
        assert_eq!(overlayable.name, "Name");
        assert_eq!(overlayable.actor, "overlay://theme");
        assert_eq!(overlayable_item.policies, policy::SIGNATURE);
    }

    #[test]
    fn parse_overlayable_requires_name() {
        assert!(test_parse(r#"<overlayable actor="overlay://theme" />"#).is_err());
        assert!(test_parse(r#"<overlayable name="Name" />"#).is_ok());
        assert!(test_parse(r#"<overlayable name="Name" actor="overlay://theme" />"#).is_ok());
    }

    #[test]
    fn parse_overlayable_bad_actor_fail() {
        assert!(test_parse(r#"<overlayable name="Name" actor="overley://theme" />"#).is_err());
    }

    #[test]
    fn parse_overlayable_policy() {
        let input = r#"
      <overlayable name="Name">
        <policy type="product">
          <item type="string" name="bar" />
        </policy>
        <policy type="system">
          <item type="string" name="fiz" />
        </policy>
        <policy type="vendor">
          <item type="string" name="fuz" />
        </policy>
        <policy type="public">
          <item type="string" name="faz" />
        </policy>
        <policy type="signature">
          <item type="string" name="foz" />
        </policy>
        <policy type="odm">
          <item type="string" name="biz" />
        </policy>
        <policy type="oem">
          <item type="string" name="buz" />
        </policy>
        <policy type="actor">
          <item type="string" name="actor" />
        </policy>
      </overlayable>"#;
        let table = test_parse(input).unwrap();

        let expectations = [
            ("string/bar", policy::PRODUCT_PARTITION),
            ("string/fiz", policy::SYSTEM_PARTITION),
            ("string/fuz", policy::VENDOR_PARTITION),
            ("string/faz", policy::PUBLIC),
            ("string/foz", policy::SIGNATURE),
            ("string/biz", policy::ODM_PARTITION),
            ("string/buz", policy::OEM_PARTITION),
            ("string/actor", policy::ACTOR_SIGNATURE),
        ];
        for (name_str, policies) in expectations {
            let entry = get_entry(&table, name_str).unwrap();
            let overlayable_item = entry.overlayable_item.as_ref().unwrap();
            assert_eq!(
                table.overlayables[overlayable_item.overlayable_index].name, "Name",
                "{name_str}"
            );
            assert_eq!(overlayable_item.policies, policies, "{name_str}");
        }
    }

    #[test]
    fn parse_overlayable_no_policy_error() {
        let input = r#"
      <overlayable name="Name">
        <item type="string" name="foo" />
      </overlayable>"#;
        assert!(test_parse(input).is_err());

        let input = r#"
      <overlayable name="Name">
        <policy>
          <item name="foo" />
        </policy>
      </overlayable>"#;
        assert!(test_parse(input).is_err());
    }

    #[test]
    fn parse_overlayable_bad_policy_error() {
        let input = r#"
      <overlayable name="Name">
        <policy type="illegal_policy">
          <item type="string" name="foo" />
        </policy>
      </overlayable>"#;
        assert!(test_parse(input).is_err());

        let input = r#"
      <overlayable name="Name">
        <policy type="product">
          <item name="foo" />
        </policy>
      </overlayable>"#;
        assert!(test_parse(input).is_err());

        let input = r#"
      <overlayable name="Name">
        <policy type="vendor">
          <item type="string" />
        </policy>
      </overlayable>"#;
        assert!(test_parse(input).is_err());
    }

    #[test]
    fn parse_overlayable_multiple_policy() {
        let input = r#"
      <overlayable name="Name">
        <policy type="vendor|public">
          <item type="string" name="foo" />
        </policy>
        <policy type="product|system">
          <item type="string" name="bar" />
        </policy>
      </overlayable>"#;
        let table = test_parse(input).unwrap();

        let entry = get_entry(&table, "string/foo").unwrap();
        assert_eq!(
            entry.overlayable_item.as_ref().unwrap().policies,
            policy::VENDOR_PARTITION | policy::PUBLIC
        );

        let entry = get_entry(&table, "string/bar").unwrap();
        assert_eq!(
            entry.overlayable_item.as_ref().unwrap().policies,
            policy::PRODUCT_PARTITION | policy::SYSTEM_PARTITION
        );
    }

    #[test]
    fn duplicate_overlayable_is_error() {
        let input = r#"
      <overlayable name="Name">
        <policy type="product">
          <item type="string" name="foo" />
          <item type="string" name="foo" />
        </policy>
      </overlayable>"#;
        assert!(test_parse(input).is_err());

        let input = r#"
      <overlayable name="Name">
        <policy type="product">
          <item type="string" name="foo" />
        </policy>
        <policy type="vendor">
          <item type="string" name="foo" />
        </policy>
      </overlayable>"#;
        assert!(test_parse(input).is_err());

        let input = r#"
      <overlayable name="Name">
        <policy type="product">
          <item type="string" name="foo" />
        </policy>
      </overlayable>
      <overlayable name="Other">
        <policy type="product">
          <item type="string" name="foo" />
        </policy>
      </overlayable>"#;
        assert!(test_parse(input).is_err());
    }

    #[test]
    fn nest_policy_in_overlayable_error() {
        let input = r#"
      <overlayable name="Name">
        <policy type="vendor|product">
          <policy type="public">
            <item type="string" name="foo" />
          </policy>
        </policy>
      </overlayable>"#;
        let err = test_parse(input).unwrap_err();
        assert!(
            err.iter().any(|e| e.contains("<policy> blocks cannot be recursively nested")),
            "{err:?}"
        );
    }

    #[test]
    fn parse_id_item() {
        let input = r#"
    <item name="foo" type="id">@id/bar</item>
    <item name="bar" type="id"/>
    <item name="baz" type="id"></item>"#;
        let table = test_parse(input).unwrap();

        assert!(matches!(
            &get_value(&table, "id/foo").unwrap().kind,
            ValueKind::Item(Item::Reference(_))
        ));
        assert!(matches!(&get_value(&table, "id/bar").unwrap().kind, ValueKind::Item(Item::Id)));
        assert!(matches!(&get_value(&table, "id/baz").unwrap().kind, ValueKind::Item(Item::Id)));

        let input = r#"
    <id name="foo2">@id/bar</id>
    <id name="bar2"/>
    <id name="baz2"></id>"#;
        let table = test_parse(input).unwrap();

        assert!(matches!(
            &get_value(&table, "id/foo2").unwrap().kind,
            ValueKind::Item(Item::Reference(_))
        ));
        assert!(matches!(&get_value(&table, "id/bar2").unwrap().kind, ValueKind::Item(Item::Id)));
        assert!(matches!(&get_value(&table, "id/baz2").unwrap().kind, ValueKind::Item(Item::Id)));

        // Reject attribute references.
        assert!(test_parse(r#"<item name="foo3" type="id">?attr/bar"</item>"#).is_err());

        // Reject non-references.
        assert!(test_parse(r#"<item name="foo4" type="id">0x7f010001</item>"#).is_err());
        assert!(test_parse(r#"<item name="foo5" type="id">@drawable/my_image</item>"#).is_err());
        assert!(
            test_parse(r#"<item name="foo6" type="id"><string name="biz"></string></item>"#)
                .is_err()
        );

        // The C++ test uses mismatched tags here, so the document itself is
        // malformed and must fail to parse.
        assert!(test_parse(r#"<public name="foo7" type="id">@id/bar7</item>"#).is_err());
    }

    #[test]
    fn parse_macro() {
        let table = test_parse(r#"<macro name="foo">12345</macro>"#).unwrap();
        match &get_value(&table, "macro/foo").unwrap().kind {
            ValueKind::Macro(value) => {
                assert_eq!(value.raw_value, "12345");
                assert_eq!(value.style_string.str, "12345");
                assert!(value.style_string.spans.is_empty());
                assert!(value.untranslatable_sections.is_empty());
                assert!(value.alias_namespaces.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_macro_untranslatable_section() {
        let input = "<macro name=\"foo\" xmlns:xliff=\"urn:oasis:names:tc:xliff:document:1.2\">\n\
This being <b><xliff:g>human</xliff:g></b> is a guest house.</macro>";
        let table = test_parse(input).unwrap();
        match &get_value(&table, "macro/foo").unwrap().kind {
            ValueKind::Macro(value) => {
                assert_eq!(value.raw_value, "\nThis being human is a guest house.");
                assert_eq!(value.style_string.str, " This being human is a guest house.");
                assert_eq!(value.style_string.spans.len(), 1);
                assert_eq!(value.style_string.spans[0].name, "b");
                assert_eq!(value.style_string.spans[0].first_char, 12);
                assert_eq!(value.style_string.spans[0].last_char, 16);
                assert_eq!(value.untranslatable_sections.len(), 1);
                assert_eq!(value.untranslatable_sections[0].start, 12);
                assert_eq!(value.untranslatable_sections[0].end, 17);
                assert!(value.alias_namespaces.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_macro_namespaces() {
        let input = "<macro name=\"foo\" xmlns:app=\"http://schemas.android.com/apk/res/android\">\n\
@app:string/foo</macro>";
        let table = test_parse(input).unwrap();
        match &get_value(&table, "macro/foo").unwrap().kind {
            ValueKind::Macro(value) => {
                assert_eq!(value.raw_value, "\n@app:string/foo");
                assert_eq!(value.style_string.str, "@app:string/foo");
                assert!(value.style_string.spans.is_empty());
                assert!(value.untranslatable_sections.is_empty());
                assert_eq!(value.alias_namespaces.len(), 1);
                assert_eq!(value.alias_namespaces[0].alias, "app");
                assert_eq!(value.alias_namespaces[0].package_name, "android");
                assert!(!value.alias_namespaces[0].is_private);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_macro_no_name_fail() {
        assert!(test_parse(r#"<macro>12345</macro>"#).is_err());
    }

    #[test]
    fn parse_macro_non_default_configuration_fail() {
        let watch_config = ConfigDescription::parse("watch").unwrap();
        let mut table = ResourceTable::new();
        let result = parse_with(
            &mut table,
            r#"<macro name="foo">12345</macro>"#,
            watch_config,
            ResourceParserOptions::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_macro_reference() {
        let table = test_parse(r#"<string name="res_string">@macro/foo</string>"#).unwrap();
        match &get_value(&table, "string/res_string").unwrap().kind {
            ValueKind::Item(Item::Reference(reference)) => {
                assert_eq!(reference.type_flags, Some(format::STRING));
                assert!(!reference.allow_raw);
            }
            other => panic!("{other:?}"),
        }

        let input = r#"<style name="foo">
                 <item name="bar">@macro/foo</item>
               </style>"#;
        let table = test_parse(input).unwrap();
        let style = get_style(&table, "style/foo");
        assert_eq!(style.entries.len(), 1);
        match &style.entries[0].value.item {
            Item::Reference(reference) => {
                assert_eq!(reference.type_flags, Some(0));
                assert!(reference.allow_raw);
            }
            other => panic!("{other:?}"),
        }
    }

    // Old AAPT allowed attributes to be defined under different
    // configurations, but ultimately stored them with the default
    // configuration.
    #[test]
    fn parse_attr_and_declare_styleable_under_config_but_record_as_no_config() {
        let watch_config = ConfigDescription::parse("watch").unwrap();
        let input = r#"
      <attr name="foo" />
      <declare-styleable name="bar">
        <attr name="baz" format="reference"/>
      </declare-styleable>"#;
        let mut table = ResourceTable::new();
        parse_with(&mut table, input, watch_config.clone(), ResourceParserOptions::default())
            .unwrap();

        assert!(get_value_for_config_product(&table, "attr/foo", &watch_config, "").is_none());
        assert!(get_value_for_config_product(&table, "attr/baz", &watch_config, "").is_none());
        assert!(
            get_value_for_config_product(&table, "styleable/bar", &watch_config, "").is_none()
        );

        assert!(get_value(&table, "attr/foo").is_some());
        assert!(get_value(&table, "attr/baz").is_some());
        assert!(get_value(&table, "styleable/bar").is_some());
    }

    #[test]
    fn parse_cdata() {
        // Double quotes should still change the state of whitespace
        // processing.
        let input =
            r#"<string name="foo">Hello<![CDATA[ "</string>' ]]>      World</string>"#;
        let table = test_parse(input).unwrap();
        assert_eq!(get_string(&table, "string/foo"), "Hello </string>'       World");

        let input = "<string name=\"foo2\"><![CDATA[Hello\n                                          World]]></string>";
        let table = test_parse(input).unwrap();
        assert_eq!(get_string(&table, "string/foo2"), "Hello World");

        // CDATA blocks have their whitespace trimmed.
        let input = r#"<string name="foo3">     <![CDATA[ text ]]>     </string>"#;
        let table = test_parse(input).unwrap();
        assert_eq!(get_string(&table, "string/foo3"), "text");

        let input = r#"<string name="foo4">     <![CDATA[]]>     </string>"#;
        let table = test_parse(input).unwrap();
        assert_eq!(get_string(&table, "string/foo4"), "");

        let input = r#"<string name="foo5">     <![CDATA[    ]]>     </string>"#;
        let table = test_parse(input).unwrap();
        assert_eq!(get_string(&table, "string/foo5"), "");

        // Single quotes must still be escaped.
        let input = r#"<string name="foo6"><![CDATA[some text and ' apostrophe]]></string>"#;
        assert!(test_parse(input).is_err());
    }

    #[test]
    fn feature_flag_attribute() {
        let mut options = ResourceParserOptions::default();
        options.feature_flags.insert(
            "flag.one".to_string(),
            FeatureFlagProperties { read_only: true, enabled: Some(true) },
        );

        // Enabled flag: the value lands as a regular value with its flag
        // status recorded.
        let input = r#"<string xmlns:android="http://schemas.android.com/apk/res/android"
                               name="foo" android:featureFlag="flag.one">on</string>"#;
        let mut table = ResourceTable::new();
        parse_with(&mut table, input, ConfigDescription::default(), options.clone()).unwrap();
        let value = get_value(&table, "string/foo").unwrap();
        assert_eq!(value.meta.flag_status, FlagStatus::Enabled);
        assert_eq!(
            value.meta.flag,
            Some(FeatureFlagAttribute { name: "flag.one".to_string(), negated: false })
        );

        // Negated flag on an enabled feature: disabled, so the value is
        // routed to flag_disabled_values.
        let input = r#"<string xmlns:android="http://schemas.android.com/apk/res/android"
                               name="bar" android:featureFlag="!flag.one">off</string>"#;
        let mut table = ResourceTable::new();
        parse_with(&mut table, input, ConfigDescription::default(), options.clone()).unwrap();
        let entry = get_entry(&table, "string/bar").unwrap();
        assert!(entry.values.is_empty());
        assert_eq!(entry.flag_disabled_values.len(), 1);

        // Unknown flag: error.
        let input = r#"<string xmlns:android="http://schemas.android.com/apk/res/android"
                               name="baz" android:featureFlag="flag.unknown">x</string>"#;
        let mut table = ResourceTable::new();
        let err =
            parse_with(&mut table, input, ConfigDescription::default(), options).unwrap_err();
        assert!(
            err.iter().any(|e| e.contains("Resource flag value undefined: flag.unknown")),
            "{err:?}"
        );
    }

    #[test]
    fn public_tag_not_allowed_with_forced_visibility() {
        let mut table = ResourceTable::new();
        let result = parse_with(
            &mut table,
            r#"<public type="string" name="foo"/>"#,
            ConfigDescription::default(),
            ResourceParserOptions {
                visibility: Some(VisibilityLevel::Public),
                ..Default::default()
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn verify_java_string_format_cases() {
        // Ported from Util_test.cpp.
        assert!(verify_java_string_format("%09.34f"));
        assert!(verify_java_string_format("%9$.34f %8$"));
        assert!(verify_java_string_format("%% %%"));
        assert!(!verify_java_string_format("%09$f %f"));
        assert!(!verify_java_string_format("%09f %08s"));
    }
}
