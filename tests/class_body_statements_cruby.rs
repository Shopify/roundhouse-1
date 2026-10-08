//! A class or module body statement Ruby runs where it stands when the
//! body loads: a call on a computed receiver, `x if cond`, `begin`.
//!
//! Core's GraphQL enum types build their values from a domain enum,
//! `Foo.values.each do |d| value d.serialize, description: d.description
//! end` in a `GraphQL::Schema::Enum` body (graphql_api/admin/
//! flow_templates_domain_type.rb), and essentials' trilogy patch wraps
//! every public method of the gem's class, `Trilogy.public_instance_
//! methods(false).each do |m| define_method(m) { … } end`. The class-body
//! walk had no carrier for either, so the emitted class lacked what they
//! define and check named each as `file_statement_unsupported` (264 rows
//! in r17). The CRuby tree writes each back in place, in source order
//! with the body's other statements; a strict target refuses it by name.
//!
//! The probe's wrapper loop forwards `*args, &block`; the trilogy patch
//! forwards `**kwargs` too, and that `super` stays refused on every
//! target by the forwarding policy (a `super` in a block has no verified
//! destination ABI), carried statement or not.
//!
//! The fixture gem `enumkit` has graphql's shape: a base class whose
//! `value` macro records on the subclass. Nothing tells analysis about
//! it; the process loads it (`ruby -renumkit`), as Core's boot loads
//! graphql. The emitted tree must print what CRuby prints.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

const GEM_LIB: &str = r#"module Enumkit
  class Enum
    def self.inherited(subclass)
      super
      subclass.instance_variable_set(:@values_list, [])
    end

    def self.values_list
      @values_list
    end

    def self.value(name, description: nil)
      raise ArgumentError, "duplicate value #{name}" if values_list.any? { |v| v.first == name }
      values_list << [name, description]
      name
    end
  end
end
"#;

/// The domain enum the type reads, an app class.
const DOMAIN: &str = r#"class FlowDomain
  Item = Struct.new(:serialize, :description)

  #: () -> Array[untyped]
  def self.values
    [Item.new("SHOP", "A shop"), Item.new("APP", "An app")]
  end
end
"#;

/// The GraphQL enum's shape, on the gem's base: in source order, a
/// literal `value`, the loop, a conditional, a `begin`, another `value`.
const DOMAIN_TYPE: &str = r#"class FlowDomainType < Enumkit::Enum
  value "FIRST", description: "first"

  FlowDomain.values.each do |d|
    value d.serialize, description: d.description
  end

  value "BETA", description: "beta" if ENV.fetch("RH_BETA", "on") == "on"
  value "NEVER" if ENV.key?("RH_UNSET_FLAG")

  begin
    value "TRIED", description: "tried"
  end

  value "LAST"

  #: () -> Array[String]
  def self.names
    values_list.map(&:first)
  end
end
"#;

/// The trilogy patch's shape on a plain app module: a statement on a
/// computed receiver before any def, one after a def that the def below
/// it overrides, and a conditional mixin.
const EXTRA: &str = r#"module Extra
  #: () -> String
  def extra = "extra"
end
"#;

const WRAPPED: &str = r#"class Wrapped
  include Extra if ENV.fetch("RH_EXTRA", "on") == "on"

  #: () -> String
  def ping = "ping"

  #: () -> String
  def pong = "pong"
end
"#;

const TRACING: &str = r#"module Tracing
  Thread.current[:tracing_loaded] = true

  #: (*untyped) -> void
  def initialize(*args)
    @traced = true
    super
  end

  Wrapped.public_instance_methods(false).each do |name|
    define_method(name) do |*args, &block|
      "traced #{super(*args, &block)}"
    end
  end

  #: () -> String
  def pong = "own pong"
end
"#;

const TRACED: &str = r#"class Traced < Wrapped
  prepend Tracing
end
"#;

/// A statement between defs, where an `alias_method` below the next def
/// replaces what the statement defined: Ruby keeps the alias.
const ALIASED: &str = r#"class Aliased
  #: () -> String
  def ping = "ping"

  (%i[pong other generated] - %i[none]).each { |n| define_method(n) { "generated #{n}" } }

  #: () -> String
  def other = "own other"

  alias_method :pong, :ping
end
"#;

/// strict_warnings' shape: a module declared in both arms of a
/// file-level `if`, each with its own def, so the file program carries
/// it inline; the taken arm's body has a conditional statement.
const INLINE_FLAGS: &str = r#"if ENV.key?("RH_UNSET_FLAG")
  module InlineFlags
    #: () -> String
    def self.mode = "unset"
  end
else
  module InlineFlags
    if ENV.fetch("RH_INLINE", "on") == "on"
      Thread.current[:inline_beta] = true
    end

    #: () -> String
    def self.mode = "set"
  end
end
"#;

const SCRIPT: &str = r#"
puts FlowDomainType.values_list.inspect
puts FlowDomainType.names.inspect
puts [Traced.new.ping, Traced.new.pong].inspect
puts Tracing.instance_methods(false).sort.inspect
puts Thread.current[:tracing_loaded].inspect
puts [Wrapped.include?(Extra), Traced.new.extra].inspect
puts [Aliased.new.pong, Aliased.new.other, Aliased.new.generated].inspect
puts [InlineFlags.mode, Thread.current[:inline_beta]].inspect
"#;

const EXPECTED: [&str; 8] = [
    r#"[["FIRST", "first"], ["SHOP", "A shop"], ["APP", "An app"], ["BETA", "beta"], ["TRIED", "tried"], ["LAST", nil]]"#,
    r#"["FIRST", "SHOP", "APP", "BETA", "TRIED", "LAST"]"#,
    r#"["traced ping", "own pong"]"#,
    "[:ping, :pong]",
    "true",
    r#"[true, "extra"]"#,
    r#"["ping", "own other", "generated generated"]"#,
    r#"["set", true]"#,
];

fn gem_home(root: &Path) -> PathBuf {
    let home = root.join("gem-home");
    let gem = home.join("gems/enumkit-1.0.0");
    std::fs::create_dir_all(gem.join("lib")).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(gem.join("lib/enumkit.rb"), GEM_LIB).unwrap();
    std::fs::write(
        home.join("specifications/enumkit-1.0.0.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"enumkit\"\n  s.version = \"1.0.0\"\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/enumkit.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    home
}

/// The fixture gem home ahead of the interpreter's own, so the emitted
/// tree's other gems still load.
fn gem_path(home: &Path) -> String {
    let out = emit_and_run::ruby().args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    format!("{}:{}", home.display(), String::from_utf8_lossy(&out.stdout))
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/models/flow_domain.rb", DOMAIN)
        .write("app/graphql/flow_domain_type.rb", DOMAIN_TYPE)
        .write("lib/extra.rb", EXTRA)
        .write("lib/wrapped.rb", WRAPPED)
        .write("lib/tracing.rb", TRACING)
        .write("lib/traced.rb", TRACED)
        .write("lib/aliased.rb", ALIASED)
        .write("lib/inline_flags.rb", INLINE_FLAGS)
}

#[test]
fn class_body_statements_run_in_place_as_in_cruby() {
    let root = std::env::temp_dir().join(format!("roundhouse-class-body-statements-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let gem_path = gem_path(&gem_home(&root));
    let native = emit_and_run::ruby()
        .env("GEM_PATH", &gem_path)
        .args(["-renumkit", "-e"])
        .arg(format!("{DOMAIN}\n{DOMAIN_TYPE}\n{EXTRA}\n{WRAPPED}\n{TRACING}\n{TRACED}\n{ALIASED}\n{INLINE_FLAGS}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(lines(&native.stdout), EXPECTED, "native control");

    let (tree, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let emitted = std::fs::read_to_string(tree.join("app/models/flow_domain_type.rb")).expect("flow_domain_type.rb");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .args(["-renumkit", "-e"])
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\n{emitted}\nerrors: {errors:#?}", tree.display());
    assert!(!stderr.contains("file_statement_not_carried"), "{stderr}");
    assert_eq!(lines(&run.stdout), EXPECTED, "emitted Ruby diverges from CRuby\n{emitted}\nerrors: {errors:#?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A target with no class body that runs a statement where it stands
/// names each one it drops.
#[test]
fn a_strict_target_refuses_each_class_body_statement_by_name() {
    let (_tree, errors) = app().emit(BuildTarget::Spinel);
    for statement in ["FlowDomain.values.each", "include Extra if", "Wrapped.public_instance_methods(false).each", "Thread.current[:tracing_loaded] = true", "(%i[pong other generated] - %i[none]).each", "if ENV.fetch(\"RH_INLINE\""] {
        assert!(
            errors.iter().any(|e| e.contains("class body statement") && e.contains(statement)),
            "{statement}: {errors:#?}"
        );
    }
}
