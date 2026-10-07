//! A named visibility change for a method the class does not define
//! itself (`public :delete_entry` over an inherited method,
//! `private_class_method :new`) keeps the class: ingest records the
//! override, the Ruby emit writes the declaration back, and the targets
//! that cannot express it refuse it by name.

use roundhouse::det::HashMap;
use std::path::{Path, PathBuf};

use roundhouse::dialect::{MethodReceiver, MethodVisibility, VisibilityOverride};
use roundhouse::ingest::{ingest_app_from_tree, ingest_library_class, ingest_library_classes, ingest_model};
use roundhouse::project::{target_files, BuildTarget};
use roundhouse::schema::Schema;

fn overrides(source: &str) -> Vec<(String, MethodReceiver, MethodVisibility)> {
    let class = ingest_library_class(source.as_bytes(), "thing.rb")
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .expect("class");
    flat(&class.visibility_overrides)
}

fn flat(overrides: &[VisibilityOverride]) -> Vec<(String, MethodReceiver, MethodVisibility)> {
    overrides
        .iter()
        .map(|o| (o.name.as_str().to_string(), o.receiver, o.visibility))
        .collect()
}

#[test]
fn a_change_without_a_local_definition_is_an_override() {
    use MethodReceiver::{Class, Instance};
    use MethodVisibility::{Private, Protected, Public};
    for (source, want) in [
        (
            "class LocalStore < BaseStore\n  private\n  public :delete_entry\n  def hidden; 2; end\nend",
            vec![("delete_entry", Instance, Public)],
        ),
        (
            "class Token\n  private_class_method :new\n  def self.build(v) = new(v)\nend",
            vec![("new", Class, Private)],
        ),
        (
            "class Loader\n  private_class_method [:load_shop, :allowed_font?]\nend",
            vec![("load_shop", Class, Private), ("allowed_font?", Class, Private)],
        ),
        (
            "class Thing\n  class << self\n    protected :inherited_helper\n  end\nend",
            vec![("inherited_helper", Class, Protected)],
        ),
        // The last change to a name wins.
        (
            "class Thing\n  private :helper\n  protected :helper\nend",
            vec![("helper", Instance, Protected)],
        ),
        // A later definition replaces the entry; it has the scope's default.
        ("class Thing\n  private :later\n  def later; 1; end\nend", vec![]),
        ("class Thing\n  private_class_method :later\n  def self.later; 1; end\nend", vec![]),
    ] {
        let want: Vec<_> = want.into_iter().map(|(n, r, v)| (n.to_string(), r, v)).collect();
        assert_eq!(overrides(source), want, "{source}");
    }
    let class = ingest_library_class(b"class Thing\n  private :later\n  def later; 1; end\nend", "thing.rb")
        .unwrap()
        .unwrap();
    assert_eq!(class.methods[0].visibility, MethodVisibility::Public);
}

#[test]
fn local_and_ambiguous_changes_keep_their_existing_rules() {
    // A local definition still carries its own visibility, not an override.
    let class = ingest_library_class(b"class Thing\n  def helper; 1; end\n  private :helper\nend", "thing.rb")
        .unwrap()
        .unwrap();
    assert!(class.visibility_overrides.is_empty());
    assert_eq!(class.methods[0].visibility, MethodVisibility::Private);
    for source in [
        "class Thing\n  def same; 1; end\n  def same; 2; end\n  private :same\nend",
        "class Thing\n  def helper; 1; end\n  private name\nend",
        "class Thing\n  private :helper if condition\nend",
        // A module has no ancestors of its own for the name to come from.
        "module Toolkit\n  def present; 1; end\n  private :missing\nend",
        // Nor does a class-method carrier: its entry would not reach the includer.
        "module Helpers\n  def present; 1; end\n  class_methods do\n    private :inherited_helper\n  end\nend",
    ] {
        assert!(ingest_library_classes(source.as_bytes(), "thing.rb").is_err(), "accepted {source}");
    }
}

#[test]
fn a_block_scoped_declaration_names_its_block() {
    // A block whose body Ruby evaluates in another object's scope is
    // named when this walk cannot model it.
    let source = "class Thing\n  Result = Class.new do\n    private\n    def eligible?; true; end\n  end\nend";
    let error = ingest_library_class(source.as_bytes(), "thing.rb").unwrap_err().to_string();
    assert!(error.contains("inside a `Class.new do … end` block are not modeled"), "{error}");
    // `Data.define(...) do … end` is modeled (`ingest::factory_class`):
    // the block is `Thing::Result`'s body, its `private` applies there
    // and does not leak into `Thing`. CRuby agrees: tests/factory_class_blocks.rs
    // compares `private_method_defined?` on the source with the emitted app.
    let source = "class Thing\n  Result = Data.define(:status) do\n    private\n    def eligible?; true; end\n  end\n  def open; 1; end\nend";
    let classes = ingest_library_classes(source.as_bytes(), "thing.rb").unwrap();
    let result = classes.iter().find(|c| c.name.0.as_str() == "Thing::Result").expect("Thing::Result");
    let eligible = result.methods.iter().find(|m| m.name.as_str() == "eligible?").expect("eligible?");
    assert_eq!(eligible.visibility, MethodVisibility::Private);
    let thing = classes.iter().find(|c| c.name.0.as_str() == "Thing").expect("Thing");
    let open = thing.methods.iter().find(|m| m.name.as_str() == "open").expect("open");
    assert_eq!(open.visibility, MethodVisibility::Public);
    let source = "class Thing\n  if ready\n    private\n    def hidden; 1; end\n  end\nend";
    let error = ingest_library_class(source.as_bytes(), "thing.rb").unwrap_err().to_string();
    assert!(error.contains("conditional or dynamic visibility declarations"), "{error}");
}

#[test]
fn model_accessors_made_private_become_overrides() {
    use MethodReceiver::Instance;
    use MethodVisibility::Private;
    for (body, want) in [
        ("private\n attr_accessor :value", vec![("value", Instance, Private), ("value=", Instance, Private)]),
        ("attr_reader :value\n private :value", vec![("value", Instance, Private)]),
        ("has_many :channels\n private :channels", vec![("channels", Instance, Private)]),
    ] {
        let source = format!("class Thing < ApplicationRecord\n{body}\nend");
        let model = ingest_model(source.as_bytes(), "thing.rb", &Schema::default(), &Default::default())
            .unwrap_or_else(|e| panic!("{source}: {e}"))
            .expect("model");
        let want: Vec<_> = want.into_iter().map(|(n, r, v)| (n.to_string(), r, v)).collect();
        assert_eq!(flat(&model.visibility_overrides), want, "{source}");
    }
}

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("app/models/base_store.rb", "class BaseStore\n  def delete_entry(key) = key\nend\n"),
        (
            "app/models/local_store.rb",
            "class LocalStore < BaseStore\n  def reset = 1\n\n  private\n\n  public :delete_entry\n\n  def hidden = 2\nend\n",
        ),
        (
            "app/models/token.rb",
            "class Token\n  private_class_method :new\n\n  def self.build(value) = new(value)\n\n  def initialize(value)\n    @value = value\n  end\n\n  attr_reader :value\nend\n",
        ),
        ("app/models/signed_token.rb", "class SignedToken < Token\n  def signed? = true\nend\n"),
        (
            "app/models/user.rb",
            "class User\n  def go = LocalStore.new.delete_entry(1)\n  def token = SignedToken.build(1).value\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny-blog")
}

fn refusals(app: &roundhouse::App, target: BuildTarget) -> Vec<String> {
    let (_, diags) = roundhouse::emit::diagnostics::scope(|| target_files(app, &root(), target));
    diags
        .into_iter()
        .filter(|d| d.message.contains("inherited_visibility_override"))
        .map(|d| d.message)
        .collect()
}

#[test]
fn ruby_writes_the_declaration_back_and_keeps_the_private_constructor_call() {
    let app = app();
    for name in ["LocalStore", "Token", "SignedToken"] {
        assert!(
            app.library_classes.iter().any(|lc| lc.name.0.as_str() == name),
            "{name} was dropped"
        );
    }
    let (files, _) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &root(), BuildTarget::Ruby));
    let files = files.expect("ruby files");
    let file = |suffix: &str| -> &str {
        &files.iter().find(|(p, _)| p.ends_with(suffix)).unwrap_or_else(|| panic!("{suffix}")).1
    };
    let store = file("app/models/local_store.rb");
    assert!(store.contains("  private :hidden\n\n  public :delete_entry\nend"), "{store}");
    let token = file("app/models/token.rb");
    assert!(token.contains("  private_class_method :new\nend"), "{token}");
    // `Token.new` is a NoMethodError once `new` is private; the bare call
    // dispatches on the receiving class, subclasses included.
    assert!(token.contains("    new(value)\n"), "{token}");
    assert!(!token.contains("Token.new"), "{token}");
    let signed = file("app/models/signed_token.rb");
    assert!(!signed.contains("def self.build"), "{signed}");
    assert_eq!(refusals(&app, BuildTarget::Ruby), Vec::<String>::new());
    assert_eq!(refusals(&app, BuildTarget::Jruby), Vec::<String>::new());
}

#[test]
fn other_targets_refuse_the_override_by_name() {
    let app = app();
    let crystal = refusals(&app, BuildTarget::Crystal);
    assert_eq!(crystal.len(), 2, "{crystal:?}");
    assert!(crystal.iter().any(|m| m.contains("LocalStore#delete_entry is made public")), "{crystal:?}");
    assert!(crystal.iter().any(|m| m.contains("Token.new is made private")), "{crystal:?}");
    let spinel = refusals(&app, BuildTarget::Spinel);
    assert_eq!(spinel.len(), 1, "{spinel:?}");
    assert!(spinel[0].contains("Token.new is private"), "{spinel:?}");
}
