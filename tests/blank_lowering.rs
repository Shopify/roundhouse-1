//! Type-directed blank-predicate lowering (`lower::apply_blank_lowering`).
//!
//! Shape tests over the grounding table: string/collection receivers
//! ground through `empty?`, nilable receivers pick up the nil guard,
//! never-blank scalars fold, classes with their own predicate keep
//! normal dispatch, and ungroundable receivers survive verbatim with a
//! `blank_unlowered` residue diagnostic.

use roundhouse::App;
use roundhouse::analyze::{Analyzer, Diagnostic};
use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_library_classes;
use roundhouse::lower::apply_blank_lowering;

fn lower_and_emit(source: &str) -> (String, Vec<Diagnostic>) {
    let classes = ingest_library_classes(source.as_bytes(), "test.rb").expect("ingest test source");
    let mut app = App::new();
    for lc in classes {
        app.library_classes.push(lc);
    }
    Analyzer::new(&app).analyze(&mut app);
    let diags = apply_blank_lowering(&mut app);
    let out = emit_library(&app)
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n");
    (out, diags)
}

#[test]
fn string_present_grounds_to_not_empty() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check
    s = "value"
    return "yes" if s.present?
    "no"
  end
end
"#,
    );
    assert!(!out.contains("present?"), "site should be grounded:\n{out}");
    assert!(out.contains("empty?"), "expected empty?-based form:\n{out}");
    assert!(
        diags.is_empty(),
        "typed receiver should not produce residue: {diags:?}"
    );
}

#[test]
fn array_blank_grounds_to_empty() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check
    a = [1, 2]
    return "none" if a.blank?
    "some"
  end
end
"#,
    );
    assert!(!out.contains("blank?"), "site should be grounded:\n{out}");
    // The ruby-family emitter's nil-safety pass may wrap the receiver
    // (`(a || "").empty?`); either surface is the grounded form.
    assert!(
        out.contains(".empty?"),
        "expected empty?-based form:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn nilable_string_present_gets_nil_guard() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check(flag)
    s = flag ? "value" : nil
    return "yes" if s.present?
    "no"
  end
end
"#,
    );
    assert!(!out.contains("present?"), "site should be grounded:\n{out}");
    assert!(
        out.contains("nil?"),
        "nilable receiver needs the nil guard:\n{out}"
    );
    assert!(out.contains("empty?"), "{out}");
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn integer_present_folds_to_true() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check
    n = 3
    return "yes" if n.present?
    "no"
  end
end
"#,
    );
    assert!(
        !out.contains("present?"),
        "never-blank scalar should fold:\n{out}"
    );
    assert!(!out.contains("empty?"), "no empty? for scalars:\n{out}");
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn string_presence_becomes_conditional() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def label(name)
    s = name.to_s
    s.presence || "anonymous"
  end
end
"#,
    );
    assert!(
        !out.contains("presence"),
        "presence should be grounded:\n{out}"
    );
    assert!(out.contains("empty?"), "{out}");
    assert!(diags.is_empty(), "{diags:?}");
}

/// No static type to ground on, so the value goes to the runtime
/// predicate rather than staying a dynamic `Object#present?` send that
/// only CRuby's core_ext reopen could serve.
#[test]
fn untyped_receiver_routes_to_the_runtime_predicate() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check(thing)
    return "yes" if thing.present?
    "no"
  end
end
"#,
    );
    assert!(
        out.contains("ActiveSupport.present?(thing)"),
        "untyped receiver should become a runtime predicate call:\n{out}"
    );
    assert!(
        !out.contains("thing.present?"),
        "the dynamic send must not survive:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

/// The runtime predicate reads its argument once, so a receiver with
/// effects grounds here where the type-directed forms must refuse it.
#[test]
fn impure_untyped_receiver_grounds_because_the_value_is_an_argument() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check(rec)
    return "yes" if rec.save!.present?
    "no"
  end
end
"#,
    );
    assert!(
        out.contains("ActiveSupport.present?(rec.save!)"),
        "impure receiver should be evaluated once as an argument:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

/// An app class that defines its own predicate keeps normal dispatch
/// even through a nilable union — the runtime helper knows nothing
/// about the app's definition and must not swallow it.
#[test]
fn nilable_own_predicate_class_still_refuses() {
    let (_out, diags) = lower_and_emit(
        r#"
class Bag
  def present?
    true
  end
end

class Util
  def check(flag)
    b = flag ? Bag.new : nil
    return "yes" if b.present?
    "no"
  end
end
"#,
    );
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code(), "blank_unlowered");
}

#[test]
fn own_predicate_class_keeps_dispatch() {
    let (out, diags) = lower_and_emit(
        r#"
class Wrapper
  def blank?
    false
  end
end

class Util
  def check
    w = Wrapper.new
    return "yes" if w.blank?
    "no"
  end
end
"#,
    );
    assert!(
        out.contains("w.blank?"),
        "class with its own predicate keeps normal dispatch:\n{out}"
    );
    assert!(
        diags.is_empty(),
        "own-predicate dispatch is not residue: {diags:?}"
    );
}

#[test]
fn indexed_read_receiver_is_reevaluable() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check(opts)
    h = { "a" => "x" }
    return "yes" if h["a"].present?
    "no"
  end
end
"#,
    );
    assert!(
        !out.contains("present?"),
        "hash-value read should ground:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

/// `params[:attachment].blank?` — typed `String?`, and for a form field
/// that is what it is; campfire's bot endpoint receives an
/// UploadedFile there, and the String grounding's `strip` on one was a
/// NoMethodError. A params read is handed to the runtime predicate,
/// which branches on the value the way Rails' `Object#blank?` does.
#[test]
fn a_params_read_is_grounded_at_runtime_whatever_its_type() {
    let (out, diags) = lower_and_emit(
        r#"
class Guard
  def check(params)
    @params = params
    return "no" if @params["attachment"].blank?
    return "no" if params[:body].blank?
    "yes"
  end
end
"#,
    );
    assert_eq!(
        out.matches("ActiveSupport.blank?(").count(),
        2,
        "both params reads should reach the runtime predicate:\n{out}"
    );
    assert!(
        !out.contains("strip"),
        "no String-shaped grounding on a params read:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

/// `{ host:, protocol: }.compact_blank` — ActiveSupport rejects on the
/// VALUE. campfire's `SetCurrentRequest#default_url_options` is this
/// Hash of nilable Strings; leaving it as a send is an AOT refusal.
#[test]
fn hash_compact_blank_rejects_on_the_value() {
    let (out, diags) = lower_and_emit(
        r#"
class Url
  def options(host, protocol)
    { host: host, protocol: protocol }.compact_blank
  end
end
"#,
    );
    assert!(
        !out.contains("compact_blank"),
        "Hash compact_blank must ground:\n{out}"
    );
    assert!(
        out.contains("reject"),
        "expected a reject over the values:\n{out}"
    );
    assert!(
        out.contains("_k") && out.contains("__cb"),
        "Hash reject binds the key and tests the value:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

/// `[ String | Content ].compact_blank` must not reject through
/// `ActiveSupport.blank?` — that helper never calls `Content#blank?`.
#[test]
fn union_with_own_predicate_compact_blank_stays_dynamic() {
    let (out, diags) = lower_and_emit(
        r#"
class Content
  def blank?
    false
  end
end

class Titles
  def of(flag)
    item = flag ? "x" : Content.new
    [ item ].compact_blank
  end
end
"#,
    );
    assert!(
        out.contains("compact_blank"),
        "own-predicate union must keep the send:\n{out}"
    );
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code(), "blank_unlowered");
}

/// An Array whose element is still an inference var used to file
/// residue and keep the send. The reject body now calls
/// `ActiveSupport.blank?`, the same runtime predicate an untyped
/// `blank?` already takes — campfire's helper
/// `[ author.name, author.bio ].compact_blank` is this shape.
#[test]
fn untyped_array_compact_blank_uses_the_runtime_predicate() {
    let (out, diags) = lower_and_emit(
        r#"
class Titles
  def of(author)
    [ author.name, author.bio ].compact_blank.join(" – ")
  end
end
"#,
    );
    assert!(
        !out.contains("compact_blank"),
        "must not keep the send:\n{out}"
    );
    assert!(
        out.contains("reject") && out.contains("ActiveSupport.blank?"),
        "expected a reject through the runtime predicate:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

/// `GraphApi::Authorization`'s shape: every caller of
/// `pre_resolve_access` is a class-body DSL call the param-site walk
/// cannot type, so `expression` kept only its `nil` default's type and
/// `expression.present?` folded to the literal `false` — every GraphQL
/// type that passed `expression:` with a docs description raised
/// `UnexpectedPublicDocsDescription` at load.
#[test]
fn nil_default_kwarg_without_typed_callers_is_not_folded_to_nil() {
    let (out, _diags) = lower_and_emit(
        r#"
class ScopeExpression
  def initialize(terms:)
    @terms = terms
  end
end

module Authorization
  def pre_resolve_access(access_scope = false, public_docs_description: nil, expression: nil, &block)
    validate(access_scope: access_scope, public_docs_description: public_docs_description, expression: expression, block: block)
  end

  def any(*scopes)
    ScopeExpression.new(terms: scopes)
  end

  private

  def validate(access_scope:, public_docs_description:, expression:, block:)
    checked = !block.nil? || access_scope.present? || expression.present?
    raise ArgumentError if !checked && public_docs_description.present?
  end
end

class BaseObject
  extend Authorization
end

class Address < BaseObject
  pre_resolve_access expression: any(:read_a, :read_b), public_docs_description: "Needs access."
end
"#,
    );
    let validate = out.split("def validate").nth(1).expect("validate emitted");
    let checked = validate.lines().nth(1).unwrap_or("");
    assert!(checked.contains("expression"), "expression.present? must not fold away:\n{out}");
    assert!(!checked.trim_end().ends_with("|| false"), "expression.present? folded to false:\n{out}");
    assert!(!validate.lines().nth(2).unwrap_or("").contains("&& false"), "public_docs_description.present? folded:\n{out}");
}

/// A class-side call through `extend` reaches the module's INSTANCE
/// `def`: the typed argument is evidence for its keyword.
#[test]
fn extended_module_method_takes_class_side_call_site_types() {
    let (out, _diags) = lower_and_emit(
        r#"
module Labeling
  def label(name: nil)
    return "none" unless name.present?
    name
  end
end

class Widget
  extend Labeling

  def self.build
    label(name: "gear")
  end
end
"#,
    );
    let label = out.split("def label").nth(1).expect("label emitted");
    assert!(label.contains("name"), "name.present? must ground on the value:\n{out}");
    assert!(!label.contains("unless true") && !label.contains("unless false"), "folded:\n{out}");
    // Typed `String?` from the site grounds statically; an unplaced site
    // would leave `name` untyped and route to the runtime predicate.
    assert!(!label.contains("ActiveSupport.present?"), "call site through extend not placed:\n{out}");
}

/// A top-level class with no `empty?` is never blank, though a
/// namespaced class of the same last segment defines one (Core's
/// `Collection` model and `Apps::Collection`): grounding it through
/// `empty?` raised on a record, which has none.
#[test]
fn same_last_segment_class_with_empty_does_not_ground_through_empty() {
    let (out, diags) = lower_and_emit(
        r#"
module Apps
  class Collection
    def empty?
      true
    end
  end
end

class Collection
end

class Util
  def check
    c = Collection.new
    return "none" if c.blank?
    "some"
  end
end
"#,
    );
    assert!(!out.contains("c.empty?") && !out.contains("c || \"\""), "the top-level Collection has no empty?:\n{out}");
    assert!(diags.is_empty(), "{diags:?}");

    let (out, _) = lower_and_emit(
        r#"
module Apps
  class Collection
    def empty?
      true
    end
  end
end

class Util
  def check
    c = Apps::Collection.new
    return "none" if c.blank?
    "some"
  end
end
"#,
    );
    assert!(out.contains("empty?"), "Apps::Collection answers through its own empty?:\n{out}");
}

/// An `Enumerable[T]` annotation names behaviour, not a class: the value
/// is an Array, a Set or a Hash, which answer `empty?`. Folding it to
/// never-blank wrote Core's `return {} if access_paths.blank?` as
/// `return {} if false`, so an empty set reached `reduce` (nil) and
/// `not_nil!` raised.
#[test]
fn enumerable_annotated_receiver_is_grounded_on_the_value() {
    let (out, diags) = lower_and_emit(
        r#"
class Util
  def check(raw)
    paths = raw #: Enumerable[Integer]
    return "none" if paths.blank?
    "some"
  end
end
"#,
    );
    assert!(!out.contains("if false"), "never-blank fold on an Enumerable:\n{out}");
    assert!(out.contains("ActiveSupport.blank?(paths)"), "expected the runtime predicate:\n{out}");
    assert!(diags.is_empty(), "{diags:?}");
}

/// A `Set` answers `empty?`.
#[test]
fn set_annotated_receiver_grounds_through_empty() {
    let (out, _diags) = lower_and_emit(
        r#"
class Util
  def check(raw)
    paths = raw #: Set[Integer]
    return "none" if paths.blank?
    "some"
  end
end
"#,
    );
    assert!(!out.contains("if false"), "never-blank fold on a Set:\n{out}");
    assert!(out.contains(".empty?"), "expected empty?-based form:\n{out}");
}

/// A parameter's scalar type is only what the analyzer saw handed in at
/// call sites, so `value.blank?` must not fold to `value.nil?`
/// (`publish_on=("")` is blank in Rails); a local the body assigns still
/// folds.
#[test]
fn inferred_scalar_param_blank_takes_runtime_predicate() {
    let (out, diags) = lower_and_emit(
        r#"
class Setter
  def publish_on=(value)
    return if value.blank?
    @at = value
  end

  def local
    n = 3
    n.blank?
  end

  def caller_site
    self.publish_on = Time.now
    self.publish_on = nil
  end
end
"#,
    );
    assert!(out.contains("ActiveSupport.blank?(value)"), "param takes the runtime predicate:\n{out}");
    assert!(!out.contains("value.nil?"), "param must not fold to nil?:\n{out}");
    assert!(!out.contains("n.blank?"), "assigned local still grounds:\n{out}");
    assert!(diags.is_empty(), "{diags:?}");
}
