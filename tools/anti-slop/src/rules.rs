use std::path::{Component, Path};

use proc_macro2::Span;
use syn::meta::ParseNestedMeta;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprMethodCall, ExprUnsafe, File, ForeignItem, ImplItem, ImplItemFn, Item,
    ItemFn, ItemImpl, Local, Macro, StmtMacro, TraitItem, TraitItemFn,
};

use crate::Diagnostic;

/// Stable rule identifiers and their intent.
pub const RULES: &[(&str, &str)] = &[
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
        "Reject production dead-code suppression and unreasoned expectations.",
    ),
];

pub(crate) struct Analyzer<'a> {
    path: &'a Path,
    lines: Vec<&'a str>,
    comment_lines: Vec<String>,
    diagnostics: Vec<Diagnostic>,
    test_depth: usize,
}

impl<'a> Analyzer<'a> {
    pub(crate) fn new(path: &'a Path, source: &'a str) -> Self {
        let file_is_test = path.components().any(|component| {
            matches!(component, Component::Normal(name) if name == "tests" || name == "benches")
        }) || path.file_name().is_some_and(|name| {
            name.to_str().is_some_and(|name| {
                matches!(name, "tests.rs" | "test.rs")
                    || name.ends_with("_test.rs")
                    || name.ends_with("_tests.rs")
            })
        });
        Self {
            path,
            lines: source.lines().collect(),
            comment_lines: extract_comment_lines(source),
            diagnostics: Vec::new(),
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

    fn has_nearby_comment(&self, span: Span, tag: &str) -> bool {
        let start = span.start();
        let line_index = start.line.saturating_sub(1);
        if let Some(comments) = self.comment_lines.get(line_index) {
            let byte_column = self
                .lines
                .get(line_index)
                .map_or(0, |line| byte_offset_for_char_column(line, start.column));
            let prefix = comments
                .get(..byte_column.min(comments.len()))
                .unwrap_or(comments);
            if contains_tag(prefix, tag) {
                return true;
            }
        }
        let mut remaining = 4;
        let mut index = start.line.saturating_sub(1);
        while index > 0 && remaining > 0 {
            index -= 1;
            remaining -= 1;
            let comments = self
                .comment_lines
                .get(index)
                .map(String::as_str)
                .unwrap_or_default();
            if contains_tag(comments, tag) {
                return true;
            }
            let line = self.lines.get(index).copied().unwrap_or_default();
            let trimmed = line.trim();
            if trimmed.is_empty() || !line_has_code(line, comments) || trimmed.starts_with("#[") {
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
    fn visit_file(&mut self, file: &'ast File) {
        let is_test = attrs_require_test(&file.attrs);
        self.test_depth += usize::from(is_test);
        visit::visit_file(self, file);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_item(&mut self, item: &'ast Item) {
        let is_test = attrs_require_test(item_attrs(item));
        self.test_depth += usize::from(is_test);
        visit::visit_item(self, item);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        let is_test = attrs_require_test(impl_item_attrs(item));
        self.test_depth += usize::from(is_test);
        visit::visit_impl_item(self, item);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        let is_test = attrs_require_test(trait_item_attrs(item));
        self.test_depth += usize::from(is_test);
        visit::visit_trait_item(self, item);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_foreign_item(&mut self, item: &'ast ForeignItem) {
        let is_test = attrs_require_test(foreign_item_attrs(item));
        self.test_depth += usize::from(is_test);
        visit::visit_foreign_item(self, item);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_expr(&mut self, expression: &'ast Expr) {
        let is_test = attrs_require_test(expr_attrs(expression));
        self.test_depth += usize::from(is_test);
        visit::visit_expr(self, expression);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_local(&mut self, local: &'ast Local) {
        let is_test = attrs_require_test(&local.attrs);
        self.test_depth += usize::from(is_test);
        visit::visit_local(self, local);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_stmt_macro(&mut self, statement: &'ast StmtMacro) {
        let is_test = attrs_require_test(&statement.attrs);
        self.test_depth += usize::from(is_test);
        visit::visit_stmt_macro(self, statement);
        self.test_depth -= usize::from(is_test);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if attribute.path().is_ident("unsafe") {
            self.check_unsafe(attribute.pound_token.span);
        }
        if !self.is_test() && attribute_dead_code_suppression_is_invalid(attribute) {
            self.report(
                attribute.pound_token.span,
                "no-dead-code-allow",
                "remove unused production code, connect it, or use a reasoned expectation",
            );
        }
        visit::visit_attribute(self, attribute);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        visit::visit_item_mod(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if let Some(unsafety) = &item.sig.unsafety {
            self.check_unsafe(unsafety.span);
        }
        visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if let Some(unsafety) = &item.sig.unsafety {
            self.check_unsafe(unsafety.span);
        }
        visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        if let Some(unsafety) = &item.sig.unsafety {
            self.check_unsafe(unsafety.span);
        }
        visit::visit_trait_item_fn(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if let Some(unsafety) = &item.unsafety {
            self.check_unsafe(unsafety.span);
        }
        visit::visit_item_impl(self, item);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
        let method = expression.method.to_string();
        if method == "unwrap" || (method == "expect" && !has_static_expect_message(expression)) {
            self.check_panic(expression.method.span());
        }
        visit::visit_expr_method_call(self, expression);
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast ExprUnsafe) {
        self.check_unsafe(expression.unsafe_token.span);
        visit::visit_expr_unsafe(self, expression);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        if let Some(segment) = mac.path.segments.last()
            && matches!(
                segment.ident.to_string().as_str(),
                "panic" | "todo" | "unimplemented" | "unreachable"
            )
        {
            self.check_panic(segment.ident.span());
        }
        visit::visit_macro(self, mac);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if let Some(unsafety) = &item.unsafety {
            self.check_unsafe(unsafety.span);
        }
        visit::visit_item_trait(self, item);
    }

    fn visit_item_foreign_mod(&mut self, item: &'ast syn::ItemForeignMod) {
        if let Some(unsafety) = &item.unsafety {
            self.check_unsafe(unsafety.span);
        }
        visit::visit_item_foreign_mod(self, item);
    }
}

fn expr_attrs(expression: &Expr) -> &[Attribute] {
    match expression {
        Expr::Array(expression) => &expression.attrs,
        Expr::Assign(expression) => &expression.attrs,
        Expr::Async(expression) => &expression.attrs,
        Expr::Await(expression) => &expression.attrs,
        Expr::Binary(expression) => &expression.attrs,
        Expr::Block(expression) => &expression.attrs,
        Expr::Break(expression) => &expression.attrs,
        Expr::Call(expression) => &expression.attrs,
        Expr::Cast(expression) => &expression.attrs,
        Expr::Closure(expression) => &expression.attrs,
        Expr::Const(expression) => &expression.attrs,
        Expr::Continue(expression) => &expression.attrs,
        Expr::Field(expression) => &expression.attrs,
        Expr::ForLoop(expression) => &expression.attrs,
        Expr::Group(expression) => &expression.attrs,
        Expr::If(expression) => &expression.attrs,
        Expr::Index(expression) => &expression.attrs,
        Expr::Infer(expression) => &expression.attrs,
        Expr::Let(expression) => &expression.attrs,
        Expr::Lit(expression) => &expression.attrs,
        Expr::Loop(expression) => &expression.attrs,
        Expr::Macro(expression) => &expression.attrs,
        Expr::Match(expression) => &expression.attrs,
        Expr::MethodCall(expression) => &expression.attrs,
        Expr::Paren(expression) => &expression.attrs,
        Expr::Path(expression) => &expression.attrs,
        Expr::Range(expression) => &expression.attrs,
        Expr::RawAddr(expression) => &expression.attrs,
        Expr::Reference(expression) => &expression.attrs,
        Expr::Repeat(expression) => &expression.attrs,
        Expr::Return(expression) => &expression.attrs,
        Expr::Struct(expression) => &expression.attrs,
        Expr::Try(expression) => &expression.attrs,
        Expr::TryBlock(expression) => &expression.attrs,
        Expr::Tuple(expression) => &expression.attrs,
        Expr::Unary(expression) => &expression.attrs,
        Expr::Unsafe(expression) => &expression.attrs,
        Expr::While(expression) => &expression.attrs,
        Expr::Yield(expression) => &expression.attrs,
        Expr::Verbatim(_) => &[],
        _ => &[],
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) => &[],
        _ => &[],
    }
}

fn impl_item_attrs(item: &ImplItem) -> &[Attribute] {
    match item {
        ImplItem::Const(item) => &item.attrs,
        ImplItem::Fn(item) => &item.attrs,
        ImplItem::Type(item) => &item.attrs,
        ImplItem::Macro(item) => &item.attrs,
        ImplItem::Verbatim(_) => &[],
        _ => &[],
    }
}

fn trait_item_attrs(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(item) => &item.attrs,
        TraitItem::Fn(item) => &item.attrs,
        TraitItem::Type(item) => &item.attrs,
        TraitItem::Macro(item) => &item.attrs,
        TraitItem::Verbatim(_) => &[],
        _ => &[],
    }
}

fn foreign_item_attrs(item: &ForeignItem) -> &[Attribute] {
    match item {
        ForeignItem::Fn(item) => &item.attrs,
        ForeignItem::Static(item) => &item.attrs,
        ForeignItem::Type(item) => &item.attrs,
        ForeignItem::Macro(item) => &item.attrs,
        ForeignItem::Verbatim(_) => &[],
        _ => &[],
    }
}

#[derive(Default)]
struct SuppressionDetails {
    hides_dead_code: bool,
    broad_group: bool,
    has_reason: bool,
}

impl SuppressionDetails {
    fn invalid(&self, is_allow: bool) -> bool {
        self.hides_dead_code && (is_allow || self.broad_group || !self.has_reason)
    }
}

fn attribute_dead_code_suppression_is_invalid(attribute: &Attribute) -> bool {
    let is_allow = attribute.path().is_ident("allow");
    let is_expect = attribute.path().is_ident("expect");
    if is_allow || is_expect {
        let mut details = SuppressionDetails::default();
        let parsed = attribute.parse_nested_meta(|meta| suppression_arg(meta, &mut details));
        return parsed.is_ok() && details.invalid(is_allow);
    }
    if !attribute.path().is_ident("cfg_attr") {
        return false;
    }

    let mut index = 0;
    let mut condition_requires_test = false;
    let mut invalid_suppression = false;
    let parsed = attribute.parse_nested_meta(|meta| {
        inspect_cfg_attr_part(
            meta,
            &mut index,
            &mut condition_requires_test,
            &mut invalid_suppression,
        )
    });
    parsed.is_ok() && !condition_requires_test && invalid_suppression
}

fn inspect_cfg_attr_meta(meta: ParseNestedMeta<'_>) -> syn::Result<(bool, bool)> {
    let mut index = 0;
    let mut condition_requires_test = false;
    let mut invalid_suppression = false;
    meta.parse_nested_meta(|nested| {
        inspect_cfg_attr_part(
            nested,
            &mut index,
            &mut condition_requires_test,
            &mut invalid_suppression,
        )
    })?;
    Ok((condition_requires_test, invalid_suppression))
}

fn inspect_cfg_attr_part(
    meta: ParseNestedMeta<'_>,
    index: &mut usize,
    condition_requires_test: &mut bool,
    invalid_suppression: &mut bool,
) -> syn::Result<()> {
    if *index == 0 {
        *condition_requires_test = cfg_meta_requires_test(meta)?;
    } else if meta.path.is_ident("allow") || meta.path.is_ident("expect") {
        let is_allow = meta.path.is_ident("allow");
        let mut details = SuppressionDetails::default();
        meta.parse_nested_meta(|nested| suppression_arg(nested, &mut details))?;
        *invalid_suppression |= details.invalid(is_allow);
    } else if meta.path.is_ident("cfg_attr") {
        let (nested_requires_test, nested_invalid) = inspect_cfg_attr_meta(meta)?;
        *invalid_suppression |= !nested_requires_test && nested_invalid;
    } else {
        consume_meta(meta)?;
    }
    *index += 1;
    Ok(())
}

fn suppression_arg(meta: ParseNestedMeta<'_>, details: &mut SuppressionDetails) -> syn::Result<()> {
    if meta.path.is_ident("dead_code")
        || meta.path.is_ident("unused")
        || meta.path.is_ident("warnings")
    {
        details.hides_dead_code = true;
        details.broad_group |= meta.path.is_ident("unused") || meta.path.is_ident("warnings");
    }
    if meta.path.is_ident("reason") && meta.input.peek(syn::Token![=]) {
        let value = meta.value()?;
        let reason: syn::Expr = value.parse()?;
        details.has_reason = matches!(
            reason,
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(message),
                ..
            }) if !message.value().trim().is_empty()
        );
        return Ok(());
    }
    consume_meta(meta)
}

fn attrs_require_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
        {
            return true;
        }
        if !attribute.path().is_ident("cfg") {
            return false;
        }
        let mut requires_test = false;
        let parsed = attribute.parse_nested_meta(|meta| {
            requires_test = cfg_meta_requires_test(meta)?;
            Ok(())
        });
        parsed.is_ok() && requires_test
    })
}

fn cfg_meta_requires_test(meta: ParseNestedMeta<'_>) -> syn::Result<bool> {
    if meta.path.is_ident("test") {
        let is_bare_test = !meta.input.peek(syn::token::Paren) && !meta.input.peek(syn::Token![=]);
        consume_meta(meta)?;
        return Ok(is_bare_test);
    }
    if meta.path.is_ident("all") {
        let mut any_child_requires_test = false;
        meta.parse_nested_meta(|nested| {
            any_child_requires_test |= cfg_meta_requires_test(nested)?;
            Ok(())
        })?;
        return Ok(any_child_requires_test);
    }
    if meta.path.is_ident("any") {
        let mut saw_child = false;
        let mut every_child_requires_test = true;
        meta.parse_nested_meta(|nested| {
            saw_child = true;
            every_child_requires_test &= cfg_meta_requires_test(nested)?;
            Ok(())
        })?;
        return Ok(saw_child && every_child_requires_test);
    }
    consume_meta(meta)?;
    Ok(false)
}

fn consume_meta(meta: ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::token::Paren) {
        return meta.parse_nested_meta(consume_meta);
    }
    if meta.input.peek(syn::Token![=]) {
        let value = meta.value()?;
        let _: syn::Expr = value.parse()?;
    }
    Ok(())
}

fn contains_tag(comments: &str, tag: &str) -> bool {
    comments.to_ascii_uppercase().contains(tag)
}

fn line_has_code(line: &str, comments: &str) -> bool {
    line.bytes()
        .zip(comments.bytes())
        .any(|(source, comment)| !source.is_ascii_whitespace() && comment == b' ')
}

fn byte_offset_for_char_column(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(offset, _)| offset)
}

#[derive(Clone, Copy)]
enum LexState {
    Normal,
    Quoted { quote: u8, escaped: bool },
    Raw { hashes: usize },
    LineComment,
    BlockComment { depth: usize },
}

fn extract_comment_lines(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut lines = vec![Vec::new()];
    let mut state = LexState::Normal;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            lines.push(Vec::new());
            state = match state {
                LexState::LineComment => LexState::Normal,
                LexState::Quoted { quote, .. } => LexState::Quoted {
                    quote,
                    escaped: false,
                },
                other => other,
            };
            index += 1;
            continue;
        }

        match state {
            LexState::Normal => {
                if bytes.get(index..index + 2) == Some(b"//") {
                    push_bytes(&mut lines, b"//", true);
                    state = LexState::LineComment;
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"/*") {
                    push_bytes(&mut lines, b"/*", true);
                    state = LexState::BlockComment { depth: 1 };
                    index += 2;
                } else if byte == b'r' {
                    if let Some((hashes, length)) = raw_string_start(bytes, index) {
                        push_bytes(&mut lines, &bytes[index..index + length], false);
                        state = LexState::Raw { hashes };
                        index += length;
                    } else {
                        push_bytes(&mut lines, &bytes[index..index + 1], false);
                        index += 1;
                    }
                } else if byte == b'"' || (byte == b'\'' && !starts_lifetime(bytes, index)) {
                    push_bytes(&mut lines, &bytes[index..index + 1], false);
                    state = LexState::Quoted {
                        quote: byte,
                        escaped: false,
                    };
                    index += 1;
                } else {
                    push_bytes(&mut lines, &bytes[index..index + 1], false);
                    index += 1;
                }
            }
            LexState::Quoted { quote, escaped } => {
                push_bytes(&mut lines, &bytes[index..index + 1], false);
                state = if escaped {
                    LexState::Quoted {
                        quote,
                        escaped: false,
                    }
                } else if byte == b'\\' {
                    LexState::Quoted {
                        quote,
                        escaped: true,
                    }
                } else if byte == quote {
                    LexState::Normal
                } else {
                    state
                };
                index += 1;
            }
            LexState::Raw { hashes } => {
                let closes = byte == b'"'
                    && bytes
                        .get(index + 1..index + 1 + hashes)
                        .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'));
                let length = if closes { hashes + 1 } else { 1 };
                push_bytes(&mut lines, &bytes[index..index + length], false);
                if closes {
                    state = LexState::Normal;
                }
                index += length;
            }
            LexState::LineComment => {
                push_bytes(&mut lines, &bytes[index..index + 1], true);
                index += 1;
            }
            LexState::BlockComment { mut depth } => {
                if bytes.get(index..index + 2) == Some(b"/*") {
                    push_bytes(&mut lines, b"/*", true);
                    depth += 1;
                    state = LexState::BlockComment { depth };
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"*/") {
                    push_bytes(&mut lines, b"*/", true);
                    depth -= 1;
                    state = if depth == 0 {
                        LexState::Normal
                    } else {
                        LexState::BlockComment { depth }
                    };
                    index += 2;
                } else {
                    push_bytes(&mut lines, &bytes[index..index + 1], true);
                    index += 1;
                }
            }
        }
    }
    lines
        .into_iter()
        .map(|line| String::from_utf8(line).expect("comment mask preserves UTF-8"))
        .collect()
}

fn push_bytes(lines: &mut [Vec<u8>], bytes: &[u8], comment: bool) {
    let line = lines.last_mut().expect("at least one line exists");
    if comment {
        line.extend_from_slice(bytes);
    } else {
        line.extend(std::iter::repeat_n(b' ', bytes.len()));
    }
}

fn raw_string_start(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let mut cursor = index + 1;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'"')).then(|| (cursor - index - 1, cursor - index + 1))
}

fn starts_lifetime(bytes: &[u8], index: usize) -> bool {
    let Some(next) = bytes.get(index + 1) else {
        return false;
    };
    (next.is_ascii_alphabetic() || *next == b'_') && bytes.get(index + 2) != Some(&b'\'')
}

fn has_static_expect_message(expression: &ExprMethodCall) -> bool {
    expression.args.first().is_some_and(|argument| {
        matches!(argument, syn::Expr::Lit(literal) if matches!(&literal.lit, syn::Lit::Str(message) if !message.value().trim().is_empty()))
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::check_source;

    fn rules_at(path: &Path, source: &str) -> Vec<&'static str> {
        check_source(path, source)
            .expect("fixture should parse")
            .into_iter()
            .map(|diagnostic| diagnostic.rule)
            .collect()
    }

    fn rules(source: &str) -> Vec<&'static str> {
        rules_at(Path::new("src/example.rs"), source)
    }

    #[test]
    fn accepts_typed_domain_code() {
        let diagnostics = rules(
            "struct User { id: String }\nfn load(user: User) -> Result<User, Error> { Ok(user) }",
        );
        assert!(diagnostics.is_empty());
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
    fn test_only_rules_are_exempt() {
        let diagnostics = rules(
            "#[cfg(all(test, unix))] mod tests {\n\
                 #[allow(dead_code)] fn helper(value: Option<u8>) { let _ = value.unwrap(); }\n\
             }",
        );
        assert!(!diagnostics.contains(&"no-dead-code-allow"));
        assert!(!diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn test_only_item_parents_are_exempt() {
        let diagnostics = rules(
            "#[cfg(test)]\n\
             impl Fixture {\n\
                 #[allow(dead_code)] fn helper(value: Option<u8>) { let _ = value.unwrap(); }\n\
                 #[allow(dead_code)] const VALUE: u8 = None.unwrap();\n\
             }\n\
             #[cfg(test)]\n\
             #[allow(dead_code)]\n\
             static FIXTURE: u8 = None.unwrap();\n\
             #[cfg(test)]\n\
             trait FixtureTrait {\n\
                 #[allow(dead_code)] fn helper(value: Option<u8>) { let _ = value.unwrap(); }\n\
             }",
        );
        assert!(!diagnostics.contains(&"no-dead-code-allow"));
        assert!(!diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn test_only_expressions_locals_and_statement_macros_are_exempt() {
        let diagnostics = rules(
            "fn fixture(value: Option<u8>) {\n\
                 #[cfg(test)]\n\
                 if true { let _ = value.unwrap(); }\n\
                 #[cfg(test)]\n\
                 let _local = value.unwrap();\n\
                 #[cfg(test)]\n\
                 panic!(\"fixture\");\n\
             }",
        );
        assert!(!diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn conventional_test_source_filenames_are_exempt() {
        let diagnostics = rules_at(
            Path::new("src/coding_agent_budget_tests.rs"),
            "#[allow(dead_code)] fn helper(value: Option<u8>) { let _ = value.unwrap(); }",
        );
        assert!(!diagnostics.contains(&"no-dead-code-allow"));
        assert!(!diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn mixed_or_negated_test_cfg_does_not_bypass_production_rules() {
        let diagnostics = rules(
            "#[cfg(not(test))]\n\
             #[allow(dead_code)]\n\
             fn production_only() { unreachable!() }\n\
             #[cfg(any(test, feature = \"fixture\"))]\n\
             #[allow(dead_code)]\n\
             fn mixed_surface() { todo!() }",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "no-dead-code-allow")
                .count(),
            2
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "require-invariant-comment-for-panics")
                .count(),
            2
        );
    }

    #[test]
    fn custom_test_cfg_key_is_not_the_builtin_test_predicate() {
        let diagnostics = rules(
            "#[cfg(test = \"fixture\")]\n\
             #[allow(dead_code)]\n\
             fn custom_cfg() { unreachable!() }",
        );
        assert!(diagnostics.contains(&"no-dead-code-allow"));
        assert!(diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn dead_code_expectation_with_a_reason_is_allowed() {
        let diagnostics =
            rules("#[expect(dead_code, reason = \"compatibility surface\")] fn retained() {}");
        assert!(!diagnostics.contains(&"no-dead-code-allow"));
    }

    #[test]
    fn unreasoned_expectations_and_unused_allows_are_rejected() {
        let diagnostics = rules(
            "#[expect(dead_code)] fn unexplained() {}\n\
             #[allow(unused)] fn broadly_suppressed() {}\n\
             #[expect(unused, reason = \"too broad\")] fn broad_expectation() {}\n\
             #[allow(warnings)] fn all_warnings_suppressed() {}",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "no-dead-code-allow")
                .count(),
            4
        );
    }

    #[test]
    fn production_capable_cfg_attr_cannot_hide_dead_code() {
        let diagnostics = rules(
            "#[cfg_attr(not(test), allow(dead_code))] fn production() {}\n\
             #[cfg_attr(test, allow(dead_code))] fn test_configuration_only() {}\n\
             #[cfg_attr(feature = \"fixture\", cfg_attr(not(test), allow(dead_code)))]\n\
             fn nested_production_suppression() {}",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "no-dead-code-allow")
                .count(),
            2
        );
    }

    #[test]
    fn dead_code_allowance_is_rejected_in_production() {
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
    fn unsafe_attributes_require_safety_comments() {
        let diagnostics = rules(
            "#[unsafe(no_mangle)]\n\
             pub extern \"C\" fn exported() {}\n\
             // SAFETY: the symbol name is uniquely owned by this crate.\n\
             #[unsafe(export_name = \"owned_symbol\")]\n\
             pub extern \"C\" fn justified() {}",
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
    fn descriptive_expect_message_documents_the_invariant() {
        let diagnostics = rules(
            "fn required(value: Option<u8>) -> u8 {\n\
                 value.expect(\"validated by the caller\")\n\
             }",
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn panic_macros_require_invariants_outside_tests() {
        let diagnostics = rules(
            "fn production() { panic!(\"broken\"); todo!(); unimplemented!(); unreachable!(); }\n\
             #[test] fn test_can_assert() { panic!(\"fixture\"); }",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|rule| **rule == "require-invariant-comment-for-panics")
                .count(),
            4
        );
    }

    #[test]
    fn safety_tag_is_case_insensitive() {
        let diagnostics = rules(
            "fn boundary() {\n\
                 // Safety: caller guarantees the pointer is valid.\n\
                 unsafe { core::hint::unreachable_unchecked(); }\n\
             }",
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn string_literals_do_not_count_as_justification_comments() {
        let diagnostics = rules(
            "fn boundary(marker: *mut &str, value: Option<u8>) {\n\
                 *marker = \"SAFETY: INVARIANT:\";\n\
                 unsafe { core::hint::unreachable_unchecked(); }\n\
                 let _ = value.unwrap();\n\
             }",
        );
        assert!(diagnostics.contains(&"require-safety-comment-for-unsafe"));
        assert!(diagnostics.contains(&"require-invariant-comment-for-panics"));
    }

    #[test]
    fn code_between_a_comment_and_boundary_breaks_proximity() {
        let diagnostics = rules(
            "fn boundary(pointer: *mut u8) {\n\
                 // SAFETY: this belongs to the earlier operation.\n\
                 *pointer = 1;\n\
                 unsafe { core::hint::unreachable_unchecked(); }\n\
             }",
        );
        assert!(diagnostics.contains(&"require-safety-comment-for-unsafe"));
    }

    #[test]
    fn real_inline_block_comments_are_recognized() {
        let diagnostics = rules(
            "fn boundary() {\n\
                 let _setup = (); /* SAFETY: the fixture establishes the precondition. */ unsafe { core::hint::unreachable_unchecked(); }\n\
             }",
        );
        assert!(!diagnostics.contains(&"require-safety-comment-for-unsafe"));
    }

    #[test]
    fn unicode_columns_preserve_same_line_comment_order() {
        let justified = rules(
            "fn boundary() { let _ = \"🦀\"; /* rationale 🦀 SAFETY: the precondition holds. */ unsafe { core::hint::unreachable_unchecked(); } }",
        );
        assert!(!justified.contains(&"require-safety-comment-for-unsafe"));

        let trailing = rules(
            "fn boundary() { let _ = \"🦀\"; unsafe { core::hint::unreachable_unchecked(); } /* 🦀 SAFETY: too late. */ }",
        );
        assert!(trailing.contains(&"require-safety-comment-for-unsafe"));
    }
}
