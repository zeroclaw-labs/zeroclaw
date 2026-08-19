use std::collections::HashSet;
use std::path::{Component, Path};

use proc_macro2::Span;
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprCast, ExprMethodCall, ExprUnsafe, File, FnArg, GenericArgument, Ident,
    ImplItemFn, ItemFn, ItemImpl, ItemType, Macro, PatType, ReturnType, TraitBound, TraitItemFn,
    Type, TypeParamBound, UseTree,
};

use crate::{Diagnostic, PolicyProfile};

/// Stable rule identifiers and their intent.
pub const RULES: &[(&str, &str)] = &[
    (
        "no-chained-casts",
        "Reject nested `as` casts that manufacture evidence in stages.",
    ),
    (
        "no-known-value-widening",
        "Reject locals that widen a known initializer to Any or dynamic JSON.",
    ),
    (
        "no-mock-macros",
        "Reject mock-generating macros in favor of real dependency seams.",
    ),
    (
        "no-shape-in-symbol-names",
        "Reject `shape` in symbol names; use a domain name.",
    ),
    (
        "no-erased-parameter-types",
        "Reject Any or dynamic JSON in function inputs.",
    ),
    (
        "no-erased-return-types",
        "Reject Any or dynamic JSON in function outputs.",
    ),
    (
        "no-erased-type-aliases",
        "Reject aliases that conceal Any or dynamic JSON.",
    ),
    (
        "no-unsafe-dictionary-types",
        "Reject string-keyed maps whose values are Any or dynamic JSON.",
    ),
    (
        "no-runtime-downcasting",
        "Reject downcasting and type-id reflection in domain code.",
    ),
    (
        "require-safety-comment-for-unsafe",
        "Require a nearby `SAFETY:` justification for unsafe code.",
    ),
    (
        "require-invariant-comment-for-panics",
        "Require a nearby `INVARIANT:` justification for production panics.",
    ),
    (
        "no-dead-code-allow",
        "Reject `allow(dead_code)` in production code.",
    ),
];

pub(crate) struct Analyzer<'a> {
    path: &'a Path,
    lines: Vec<&'a str>,
    diagnostics: Vec<Diagnostic>,
    json_value_names: HashSet<String>,
    erased_alias_names: HashSet<String>,
    profile: PolicyProfile,
    test_depth: usize,
}

impl<'a> Analyzer<'a> {
    pub(crate) fn new(
        path: &'a Path,
        source: &'a str,
        file: &File,
        profile: PolicyProfile,
    ) -> Self {
        let mut json_value_names = HashSet::from(["JsonValue".to_string()]);
        collect_json_imports(file, &mut json_value_names);
        let erased_alias_names = collect_erased_aliases(file, &json_value_names, profile);
        let file_is_test = path.components().any(|component| {
            matches!(component, Component::Normal(name) if name == "tests" || name == "benches")
        }) || path.file_name().is_some_and(|name| {
            matches!(name.to_str(), Some("tests.rs" | "test.rs"))
        });
        Self {
            path,
            lines: source.lines().collect(),
            diagnostics: Vec::new(),
            json_value_names,
            erased_alias_names,
            profile,
            test_depth: usize::from(file_is_test),
        }
    }

    pub(crate) fn finish(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    fn report(&mut self, span: Span, rule: &'static str, message: &'static str) {
        let start = span.start();
        self.diagnostics.push(Diagnostic {
            path: self.path.to_path_buf(),
            line: start.line.max(1),
            column: start.column.saturating_add(1),
            rule,
            message,
        });
    }

    fn is_test(&self) -> bool {
        self.test_depth > 0
    }

    fn check_signature(
        &mut self,
        function_name: &Ident,
        inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
        output: &ReturnType,
    ) {
        for input in inputs {
            let FnArg::Typed(argument) = input else {
                continue;
            };
            if type_contains_erased(
                &argument.ty,
                &self.json_value_names,
                &self.erased_alias_names,
                self.profile,
            ) {
                self.report(
                    argument.ty.span(),
                    "no-erased-parameter-types",
                    "parse erased input at its I/O boundary and accept a named domain type",
                );
            }
        }
        if let ReturnType::Type(_, ty) = output
            && !(self.profile == PolicyProfile::ZeroClaw && function_name == "as_any")
            && type_contains_erased(
                ty,
                &self.json_value_names,
                &self.erased_alias_names,
                self.profile,
            )
        {
            self.report(
                ty.span(),
                "no-erased-return-types",
                "parse erased output at its I/O boundary and return a named domain type",
            );
        }
    }

    fn check_ident(&mut self, ident: &Ident) {
        if self.profile == PolicyProfile::Strict
            && ident.to_string().to_ascii_lowercase().contains("shape")
        {
            self.report(
                ident.span(),
                "no-shape-in-symbol-names",
                "replace `shape` with the domain concept this symbol owns",
            );
        }
    }

    fn has_nearby_comment(&self, span: Span, tag: &str) -> bool {
        let start = span.start();
        if let Some(line) = self.lines.get(start.line.saturating_sub(1)) {
            let prefix = line.get(..start.column.min(line.len())).unwrap_or(line);
            if contains_comment_tag(prefix, tag, self.profile) {
                return true;
            }
        }
        let mut remaining = 4;
        let mut index = start.line.saturating_sub(1);
        while index > 0 && remaining > 0 {
            index -= 1;
            remaining -= 1;
            let line = self.lines.get(index).copied().unwrap_or_default().trim();
            if contains_comment_tag(line, tag, self.profile) {
                return true;
            }
            if line.is_empty()
                || line.starts_with("//")
                || line.starts_with("/*")
                || line.starts_with('*')
                || line.starts_with("#[")
            {
                continue;
            }
            break;
        }
        false
    }

    fn check_unsafe(&mut self, span: Span) {
        if !self.has_nearby_comment(span, "SAFETY:") {
            self.report(
                span,
                "require-safety-comment-for-unsafe",
                "state the checked invariant in a nearby `SAFETY:` comment",
            );
        }
    }

    fn check_panic(&mut self, span: Span) {
        if !self.is_test() && !self.has_nearby_comment(span, "INVARIANT:") {
            self.report(
                span,
                "require-invariant-comment-for-panics",
                "propagate the error or justify why panic is impossible with `INVARIANT:`",
            );
        }
    }
}

impl<'ast> Visit<'ast> for Analyzer<'_> {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if !self.is_test() && attribute.path().is_ident("allow") {
            let tokens = attribute
                .meta
                .require_list()
                .ok()
                .map(|list| list.tokens.to_string());
            if tokens
                .is_some_and(|tokens| tokens.split_whitespace().any(|token| token == "dead_code"))
            {
                self.report(
                    attribute.span(),
                    "no-dead-code-allow",
                    "remove unused production code, connect it, or track the missing behavior",
                );
            }
        }
        if attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "automock")
        {
            self.report(
                attribute.span(),
                "no-mock-macros",
                "replace generated mocks with a faithful implementation of the real interface",
            );
        }
        visit::visit_attribute(self, attribute);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        self.check_ident(&item.ident);
        let is_test = attrs_are_test(&item.attrs);
        self.test_depth += usize::from(is_test);
        visit::visit_item_mod(self, item);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.check_ident(&item.sig.ident);
        let is_test = attrs_are_test(&item.attrs);
        self.test_depth += usize::from(is_test);
        self.check_signature(&item.sig.ident, &item.sig.inputs, &item.sig.output);
        if let Some(unsafety) = item.sig.unsafety {
            self.check_unsafe(unsafety.span());
        }
        visit::visit_item_fn(self, item);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        self.check_ident(&item.sig.ident);
        let is_test = attrs_are_test(&item.attrs);
        self.test_depth += usize::from(is_test);
        self.check_signature(&item.sig.ident, &item.sig.inputs, &item.sig.output);
        if let Some(unsafety) = item.sig.unsafety {
            self.check_unsafe(unsafety.span());
        }
        visit::visit_impl_item_fn(self, item);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        self.check_ident(&item.sig.ident);
        self.check_signature(&item.sig.ident, &item.sig.inputs, &item.sig.output);
        if let Some(unsafety) = item.sig.unsafety {
            self.check_unsafe(unsafety.span());
        }
        visit::visit_trait_item_fn(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if let Some(unsafety) = item.unsafety {
            self.check_unsafe(unsafety.span());
        }
        visit::visit_item_impl(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        self.check_ident(&item.ident);
        if type_contains_erased(
            &item.ty,
            &self.json_value_names,
            &self.erased_alias_names,
            self.profile,
        ) {
            self.report(
                item.ident.span(),
                "no-erased-type-aliases",
                "do not hide erased data behind an alias; parse it into an owner type",
            );
        }
        visit::visit_item_type(self, item);
    }

    fn visit_type(&mut self, ty: &'ast Type) {
        if is_unsafe_dictionary(
            ty,
            &self.json_value_names,
            &self.erased_alias_names,
            self.profile,
        ) {
            self.report(
                ty.span(),
                "no-unsafe-dictionary-types",
                "use an owner/schema-derived value type and parse external values before insertion",
            );
        }
        visit::visit_type(self, ty);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let syn::Pat::Type(PatType { ty, .. }) = &local.pat
            && local.init.is_some()
            && type_contains_erased(
                ty,
                &self.json_value_names,
                &self.erased_alias_names,
                self.profile,
            )
        {
            self.report(
                ty.span(),
                "no-known-value-widening",
                "preserve the initializer's concrete type instead of erasing known evidence",
            );
        }
        visit::visit_local(self, local);
    }

    fn visit_expr_cast(&mut self, expression: &'ast ExprCast) {
        if matches!(strip_groups(&expression.expr), Expr::Cast(_)) {
            self.report(
                expression.as_token.span(),
                "no-chained-casts",
                "replace staged casts with a typed conversion that establishes the invariant once",
            );
        }
        visit::visit_expr_cast(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
        let method = expression.method.to_string();
        if matches!(
            method.as_str(),
            "downcast" | "downcast_ref" | "downcast_mut" | "type_id"
        ) && self.profile == PolicyProfile::Strict
        {
            self.report(
                expression.method.span(),
                "no-runtime-downcasting",
                "decode at the boundary and branch on a domain enum or trait method",
            );
        }
        if method == "unwrap"
            || (method == "expect"
                && (self.profile == PolicyProfile::Strict
                    || !has_static_expect_message(expression)))
        {
            self.check_panic(expression.method.span());
        }
        visit::visit_expr_method_call(self, expression);
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast ExprUnsafe) {
        self.check_unsafe(expression.unsafe_token.span());
        visit::visit_expr_unsafe(self, expression);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        if let Some(segment) = mac.path.segments.last() {
            let name = segment.ident.to_string();
            if name == "mock" {
                self.report(
                    segment.ident.span(),
                    "no-mock-macros",
                    "replace generated mocks with a faithful implementation of the real interface",
                );
            }
            if matches!(
                name.as_str(),
                "panic" | "todo" | "unimplemented" | "unreachable"
            ) {
                self.check_panic(segment.ident.span());
            }
        }
        visit::visit_macro(self, mac);
    }

    fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
        self.check_ident(&pattern.ident);
        visit::visit_pat_ident(self, pattern);
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if let Some(ident) = &field.ident {
            self.check_ident(ident);
        }
        visit::visit_field(self, field);
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        self.check_ident(&variant.ident);
        visit::visit_variant(self, variant);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.check_ident(&item.ident);
        visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.check_ident(&item.ident);
        visit::visit_item_enum(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        self.check_ident(&item.ident);
        if let Some(unsafety) = item.unsafety {
            self.check_unsafe(unsafety.span());
        }
        visit::visit_item_trait(self, item);
    }

    fn visit_item_foreign_mod(&mut self, item: &'ast syn::ItemForeignMod) {
        if let Some(unsafety) = item.unsafety {
            self.check_unsafe(unsafety.span());
        }
        visit::visit_item_foreign_mod(self, item);
    }

    fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
        self.check_ident(&item.ident);
        visit::visit_item_const(self, item);
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        self.check_ident(&item.ident);
        visit::visit_item_static(self, item);
    }
}

fn attrs_are_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg") && {
                let mut found = false;
                let parsed =
                    attribute.parse_nested_meta(|meta| cfg_meta_contains_test(meta, &mut found));
                parsed.is_ok() && found
            })
    })
}

fn cfg_meta_contains_test(meta: ParseNestedMeta<'_>, found: &mut bool) -> syn::Result<()> {
    if meta.path.is_ident("test") {
        *found = true;
        return Ok(());
    }
    if meta.input.peek(syn::token::Paren) {
        return meta.parse_nested_meta(|nested| cfg_meta_contains_test(nested, found));
    }
    if meta.input.peek(syn::Token![=]) {
        let value = meta.value()?;
        let _: syn::Expr = value.parse()?;
    }
    Ok(())
}

fn strip_groups(mut expression: &Expr) -> &Expr {
    loop {
        expression = match expression {
            Expr::Group(group) => &group.expr,
            Expr::Paren(paren) => &paren.expr,
            _ => return expression,
        };
    }
}

fn contains_comment_tag(line: &str, tag: &str, profile: PolicyProfile) -> bool {
    match profile {
        PolicyProfile::Strict => line.contains(tag),
        PolicyProfile::ZeroClaw => line.to_ascii_uppercase().contains(tag),
    }
}

fn has_static_expect_message(expression: &ExprMethodCall) -> bool {
    expression.args.first().is_some_and(|argument| {
        matches!(argument, Expr::Lit(literal) if matches!(&literal.lit, syn::Lit::Str(message) if !message.value().trim().is_empty()))
    })
}

fn type_contains_erased(
    ty: &Type,
    json_names: &HashSet<String>,
    erased_aliases: &HashSet<String>,
    profile: PolicyProfile,
) -> bool {
    match ty {
        Type::Array(ty) => type_contains_erased(&ty.elem, json_names, erased_aliases, profile),
        Type::BareFn(function) => {
            function.inputs.iter().any(|input| {
                type_contains_erased(&input.ty, json_names, erased_aliases, profile)
            }) || matches!(&function.output, ReturnType::Type(_, ty) if type_contains_erased(ty, json_names, erased_aliases, profile))
        }
        Type::Group(ty) => type_contains_erased(&ty.elem, json_names, erased_aliases, profile),
        Type::ImplTrait(ty) => bounds_contain_any(&ty.bounds),
        Type::Paren(ty) => type_contains_erased(&ty.elem, json_names, erased_aliases, profile),
        Type::Path(ty) => {
            path_is_erased(&ty.path, json_names, erased_aliases, profile)
                || ty.path.segments.iter().any(|segment| match &segment.arguments {
                    syn::PathArguments::AngleBracketed(arguments) => arguments.args.iter().any(|argument| {
                        matches!(argument, GenericArgument::Type(ty) if type_contains_erased(ty, json_names, erased_aliases, profile))
                    }),
                    syn::PathArguments::Parenthesized(arguments) => {
                        arguments.inputs.iter().any(|ty| type_contains_erased(ty, json_names, erased_aliases, profile))
                            || matches!(&arguments.output, ReturnType::Type(_, ty) if type_contains_erased(ty, json_names, erased_aliases, profile))
                    }
                    syn::PathArguments::None => false,
                })
        }
        Type::Ptr(ty) => type_contains_erased(&ty.elem, json_names, erased_aliases, profile),
        Type::Reference(ty) => type_contains_erased(&ty.elem, json_names, erased_aliases, profile),
        Type::Slice(ty) => type_contains_erased(&ty.elem, json_names, erased_aliases, profile),
        Type::TraitObject(ty) => bounds_contain_any(&ty.bounds),
        Type::Tuple(ty) => ty
            .elems
            .iter()
            .any(|ty| type_contains_erased(ty, json_names, erased_aliases, profile)),
        _ => false,
    }
}

fn bounds_contain_any(
    bounds: &syn::punctuated::Punctuated<TypeParamBound, syn::token::Plus>,
) -> bool {
    bounds.iter().any(|bound| {
        matches!(bound, TypeParamBound::Trait(TraitBound { path, .. }) if path.segments.last().is_some_and(|segment| segment.ident == "Any"))
    })
}

fn path_is_erased(
    path: &syn::Path,
    json_names: &HashSet<String>,
    erased_aliases: &HashSet<String>,
    profile: PolicyProfile,
) -> bool {
    let Some(last) = path.segments.last() else {
        return false;
    };
    let name = last.ident.to_string();
    if name == "Any" || erased_aliases.contains(&name) {
        return true;
    }
    if profile == PolicyProfile::ZeroClaw {
        return false;
    }
    if json_names.contains(&name) {
        return true;
    }
    name == "Value"
        && path
            .segments
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|segment| segment.ident == "serde_json")
}

fn is_unsafe_dictionary(
    ty: &Type,
    json_names: &HashSet<String>,
    erased_aliases: &HashSet<String>,
    profile: PolicyProfile,
) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    if !matches!(
        segment.ident.to_string().as_str(),
        "HashMap" | "BTreeMap" | "IndexMap" | "DashMap"
    ) {
        return false;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return false;
    };
    let types: Vec<_> = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .collect();
    types.len() >= 2
        && is_string_type(types[0])
        && type_contains_erased(types[1], json_names, erased_aliases, profile)
}

fn is_string_type(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "String"),
        Type::Reference(reference) => {
            matches!(&*reference.elem, Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "str"))
        }
        _ => false,
    }
}

fn collect_json_imports(file: &File, names: &mut HashSet<String>) {
    struct Collector<'a>(&'a mut HashSet<String>);
    impl<'ast> Visit<'ast> for Collector<'_> {
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            collect_use_tree(Vec::new(), &item.tree, self.0);
            visit::visit_item_use(self, item);
        }
    }
    Collector(names).visit_file(file);
}

fn collect_use_tree(prefix: Vec<String>, tree: &UseTree, names: &mut HashSet<String>) {
    match tree {
        UseTree::Path(path) => {
            let mut prefix = prefix;
            prefix.push(path.ident.to_string());
            collect_use_tree(prefix, &path.tree, names);
        }
        UseTree::Name(name) => {
            if prefix.last().is_some_and(|part| part == "serde_json") && name.ident == "Value" {
                names.insert(name.ident.to_string());
            }
        }
        UseTree::Rename(rename) => {
            if prefix.last().is_some_and(|part| part == "serde_json") && rename.ident == "Value" {
                names.insert(rename.rename.to_string());
            }
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree(prefix.clone(), item, names);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn collect_erased_aliases(
    file: &File,
    json_names: &HashSet<String>,
    profile: PolicyProfile,
) -> HashSet<String> {
    #[derive(Default)]
    struct Collector(Vec<(String, Type)>);
    impl<'ast> Visit<'ast> for Collector {
        fn visit_item_type(&mut self, item: &'ast ItemType) {
            self.0.push((item.ident.to_string(), (*item.ty).clone()));
            visit::visit_item_type(self, item);
        }
    }
    let mut collector = Collector::default();
    collector.visit_file(file);
    let mut aliases = HashSet::new();
    loop {
        let before = aliases.len();
        for (name, ty) in &collector.0 {
            if type_contains_erased(ty, json_names, &aliases, profile) {
                aliases.insert(name.clone());
            }
        }
        if aliases.len() == before {
            return aliases;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::{PolicyProfile, check_source, check_source_with_profile};

    fn rules(source: &str) -> Vec<&'static str> {
        check_source(Path::new("src/example.rs"), source)
            .expect("fixture should parse")
            .into_iter()
            .map(|diagnostic| diagnostic.rule)
            .collect()
    }

    fn zeroclaw_rules(source: &str) -> Vec<&'static str> {
        check_source_with_profile(Path::new("src/example.rs"), source, PolicyProfile::ZeroClaw)
            .expect("fixture should parse")
            .into_iter()
            .map(|diagnostic| diagnostic.rule)
            .collect()
    }

    #[test]
    fn accepts_typed_domain_code() {
        let diagnostics = rules(
            "struct User { id: String }\nfn load(user: User) -> Result<User, Error> { Ok(user) }",
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn reports_erased_boundaries_aliases_and_dictionaries() {
        let diagnostics = rules(
            "use serde_json::Value as Json;\n\
             use std::collections::HashMap;\n\
             type Payload = Json;\n\
             fn load(input: Box<dyn std::any::Any>) -> Payload { todo!() }\n\
             fn metadata() -> HashMap<String, Json> { todo!() }",
        );
        assert!(diagnostics.contains(&"no-erased-parameter-types"));
        assert!(diagnostics.contains(&"no-erased-return-types"));
        assert!(diagnostics.contains(&"no-erased-type-aliases"));
        assert!(diagnostics.contains(&"no-unsafe-dictionary-types"));
    }

    #[test]
    fn reports_widening_downcasts_chained_casts_and_shape_names() {
        let diagnostics = rules(
            "fn decode(user_shape: User) {\n\
                 let value: Box<dyn std::any::Any> = Box::new(user_shape);\n\
                 let _ = value.downcast_ref::<User>();\n\
                 let _ = 1_u8 as u16 as u32;\n\
             }",
        );
        assert!(diagnostics.contains(&"no-known-value-widening"));
        assert!(diagnostics.contains(&"no-runtime-downcasting"));
        assert!(diagnostics.contains(&"no-chained-casts"));
        assert!(diagnostics.contains(&"no-shape-in-symbol-names"));
    }

    #[test]
    fn requires_safety_and_invariant_comments() {
        let diagnostics = rules(
            "fn broken(value: Option<u8>) {\n\
                 let _ = value.unwrap();\n\
                 unsafe { core::ptr::read(core::ptr::null()); }\n\
             }\n\
             fn justified(value: Option<u8>) {\n\
                 // INVARIANT: the caller established presence.\n\
                 let _ = value.unwrap();\n\
                 // SAFETY: the pointer is valid in the real implementation.\n\
                 unsafe { core::hint::unreachable_unchecked(); }\n\
             }",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "require-invariant-comment-for-panics")
                .count(),
            1
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "require-safety-comment-for-unsafe")
                .count(),
            1
        );
    }

    #[test]
    fn production_only_rules_ignore_test_modules() {
        let diagnostics = rules(
            "#[cfg(all(test, unix))] mod tests {\n\
                 #[allow(dead_code)] fn helper(value: Option<u8>) { let _ = value.unwrap(); }\n\
             }",
        );
        assert!(!diagnostics.contains(&"no-dead-code-allow"));
        assert!(!diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn mock_generation_is_rejected_even_in_tests() {
        let diagnostics = rules("#[cfg(test)] mod tests { mock! { Store {} } }");
        assert!(diagnostics.contains(&"no-mock-macros"));
    }

    #[test]
    fn dead_code_suppression_is_rejected_in_production() {
        let diagnostics = rules("#[allow(dead_code)] fn disconnected() {}");
        assert!(diagnostics.contains(&"no-dead-code-allow"));
    }

    #[test]
    fn unsafe_traits_and_foreign_blocks_require_safety_comments() {
        let diagnostics = rules(
            "unsafe trait Marker {}\n\
             unsafe extern \"C\" { fn boundary(); }",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "require-safety-comment-for-unsafe")
                .count(),
            2
        );
    }

    #[test]
    fn zeroclaw_profile_preserves_wire_json_and_schema_vocabulary() {
        let diagnostics = zeroclaw_rules(
            "use serde_json::Value;\n\
             struct RequestShape;\n\
             fn dispatch(input: Value) -> Value {\n\
                 let payload: Value = input;\n\
                 payload\n\
             }",
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn zeroclaw_profile_allows_documented_expect_and_test_downcast() {
        let diagnostics = zeroclaw_rules(
            "fn required(value: Option<u8>) -> u8 {\n\
                 value.expect(\"validated by the caller\")\n\
             }\n\
             #[cfg(test)] mod tests {\n\
                 fn typed(error: &anyhow::Error) { let _ = error.downcast_ref::<Error>(); }\n\
             }",
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn zeroclaw_profile_preserves_canonical_any_capability_seam() {
        let diagnostics = zeroclaw_rules(
            "trait Observer { fn as_any(&self) -> &dyn std::any::Any; }\n\
             fn backend(observer: &dyn Observer) {\n\
                 let _ = observer.as_any().downcast_ref::<Backend>();\n\
             }",
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn zeroclaw_profile_keeps_any_unwrap_and_dead_code_guards() {
        let diagnostics = zeroclaw_rules(
            "#[allow(dead_code)] fn decode(value: Box<dyn std::any::Any>) {\n\
                 let _ = Some(value).unwrap();\n\
             }",
        );
        assert!(diagnostics.contains(&"no-dead-code-allow"));
        assert!(diagnostics.contains(&"no-erased-parameter-types"));
        assert!(diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn zeroclaw_profile_accepts_case_insensitive_safety_tag() {
        let diagnostics = zeroclaw_rules(
            "fn boundary() {\n\
                 // Safety: caller guarantees the pointer is valid.\n\
                 unsafe { core::hint::unreachable_unchecked(); }\n\
             }",
        );
        assert!(diagnostics.is_empty());
    }
}
