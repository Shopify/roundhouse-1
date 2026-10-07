//! Which methods of an `app/helpers/*_helper.rb` module Rails exposes.
//!
//! Rails mixes a helper module into the view context (and into any class
//! that `include`s it), so what a template can call bare is the module's
//! INSTANCE surface: plain `def`s, plus the copies `module_function` and
//! `extend self` make. A method defined on the module's singleton —
//! `def self.x`, a `class << self` body, a Concern's `class_methods do` /
//! `module ClassMethods` — is reachable only as `Helper.x` and never as a
//! bare call.
//!
//! The library-class ingest lands both kinds as `MethodReceiver::Class`
//! (that is the call shape every Ruby target uses for them), so it can't
//! tell them apart afterwards. This module reads the origin off the
//! source: the name-token offset of every `def <recv>.x`, and the byte
//! ranges of every singleton scope. A `Class` method whose name token
//! falls on one of those is singleton-defined.
//!
//! Core's `MoneyHelper` is why this exists: its private
//! `class << self; def format(money, …)` entered the bare-call registry,
//! and every `format("%06i", n)` in every model was rewritten to
//! `MoneyHelper.format(…)`.

use crate::dialect::{MethodDef, MethodReceiver};

/// Source positions of singleton-defined methods in one file.
#[derive(Default)]
pub(crate) struct SingletonOrigins {
    /// Name-token start offsets of `def <recv>.x`.
    receiver_defs: Vec<usize>,
    /// `[start, end)` of each `class << self`, `module ClassMethods`, and
    /// `class_methods do … end` body.
    scopes: Vec<(usize, usize)>,
}

impl SingletonOrigins {
    pub(crate) fn of(source: &[u8]) -> Self {
        let parsed = ruby_prism::parse(source);
        let mut out = SingletonOrigins::default();
        ruby_prism::Visit::visit(&mut out, &parsed.node());
        out
    }

    /// Was `m` defined on the module's singleton, as opposed to being an
    /// instance method or a `module_function` / `extend self` copy of one?
    ///
    /// Read off the name token; a method a lowering synthesized has none,
    /// so its body's start stands in (an `attr_reader` inside
    /// `class << self` carries the macro's span). A method with neither is
    /// not claimed: the index keeps the pre-existing behavior for it.
    pub(crate) fn is_singleton_defined(&self, m: &MethodDef) -> bool {
        if m.receiver != MethodReceiver::Class {
            return false;
        }
        let at = if !m.name_span.is_synthetic() {
            m.name_span.start as usize
        } else if !m.body.span.is_synthetic() {
            m.body.span.start as usize
        } else {
            return false;
        };
        self.receiver_defs.contains(&at)
            || self.scopes.iter().any(|(start, end)| *start <= at && at < *end)
    }
}

/// [`SingletonOrigins`] for methods ingested from any registered file —
/// a module a helper `include`s comes from `lib/` or `app/models/`,
/// ingested before the helpers walk, so its text is read back from the
/// source registry by the method's own span.
#[derive(Default)]
pub(crate) struct RegisteredOrigins {
    by_file: std::collections::BTreeMap<u32, Option<SingletonOrigins>>,
}

impl RegisteredOrigins {
    pub(crate) fn is_singleton_defined(&mut self, m: &MethodDef) -> bool {
        if m.receiver != MethodReceiver::Class {
            return false;
        }
        let file = if !m.name_span.is_synthetic() { m.name_span.file } else { m.body.span.file };
        self.by_file
            .entry(file.0)
            .or_insert_with(|| {
                super::sources::text_of_file(file).map(|text| SingletonOrigins::of(text.as_bytes()))
            })
            .as_ref()
            .is_some_and(|origins| origins.is_singleton_defined(m))
    }
}

impl<'pr> ruby_prism::Visit<'pr> for SingletonOrigins {
    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        if node.receiver().is_some() {
            self.receiver_defs.push(node.name_loc().start_offset());
        }
        ruby_prism::visit_def_node(self, node);
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        let loc = node.location();
        self.scopes.push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_singleton_class_node(self, node);
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        let is_class_methods = node
            .constant_path()
            .as_constant_read_node()
            .is_some_and(|c| c.name().as_slice() == b"ClassMethods");
        if is_class_methods {
            let loc = node.location();
            self.scopes.push((loc.start_offset(), loc.end_offset()));
        }
        ruby_prism::visit_module_node(self, node);
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if node.receiver().is_none() && node.name().as_slice() == b"class_methods" {
            if let Some(block) = node.block() {
                let loc = block.location();
                self.scopes.push((loc.start_offset(), loc.end_offset()));
            }
        }
        ruby_prism::visit_call_node(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn singleton_names(source: &str) -> Vec<(String, bool)> {
        let origins = SingletonOrigins::of(source.as_bytes());
        let classes =
            crate::ingest::library_class::ingest_library_classes(source.as_bytes(), "app/helpers/x_helper.rb")
                .expect("ingest");
        classes
            .iter()
            .flat_map(|lc| lc.methods.iter())
            .filter(|m| m.receiver == MethodReceiver::Class)
            .map(|m| (m.name.as_str().to_string(), origins.is_singleton_defined(m)))
            .collect()
    }

    #[test]
    fn singleton_defs_are_told_apart_from_module_function_copies() {
        let source = "module XHelper\n  class << self\n    private\n    def format(m) m end\n  end\n\n  def self.direct; end\n\n  def shown; end\n\n  module_function\n\n  def copied; end\nend\n";
        assert_eq!(
            singleton_names(source),
            vec![
                ("format".to_string(), true),
                ("direct".to_string(), true),
                ("copied".to_string(), false),
            ]
        );
    }

    #[test]
    fn extend_self_copies_stay_instance_visible() {
        let source = "module XHelper\n  extend self\n\n  def shared; end\n\n  def self.only_class; end\nend\n";
        assert_eq!(
            singleton_names(source),
            vec![("shared".to_string(), false), ("only_class".to_string(), true)]
        );
    }
}
