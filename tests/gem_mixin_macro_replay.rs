//! A class body calls a class macro a loaded gem's mixin gives it.
//!
//! Core's `class Entitlements; include SmartProperties; property! :plan,
//! accepts: String`: the smart_properties gem's `included(base)` hook
//! runs `base.extend(ClassMethods)`, which Tapioca's RBI writes as
//! `mixes_in_class_methods ::SmartProperties::ClassMethods`, so Ruby
//! runs `property!` on the class through the gem. The emitted tree loads
//! the gem natively, yet dropped every such call as "not modelled"
//! (about 4,700 in Core), and the classes lacked all their properties.
//!
//! The fixture is a small gem named `smart_properties` installed into a
//! scratch gem home, with the real gem's mixin shape (an `included` hook
//! extending the includer with `ClassMethods`, whose `property` defines
//! the accessors): the CRuby tree requires it because the lockfile pins
//! it (`project.rs::LOCK_GATED_REQUIRES`), and its RBI declares it. The
//! emitted tree must print what CRuby prints, line for line.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::det::HashMap;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;

const GEM_LIB: &str = r##"module SmartProperties
  VERSION = "1.17.0"

  class Error < ArgumentError; end

  class MissingValueError < Error
    def initialize(owner, name)
      super("#{owner.class.name} requires the property #{name} to be set")
    end
  end

  module ClassMethods
    def properties
      @_smart_properties ||= begin
        inherited = respond_to?(:superclass) && superclass.respond_to?(:properties) ? superclass.properties : {}
        inherited.dup
      end
    end

    def property(name, **options)
      properties[name] = options
      define_method(name) do
        @_values.fetch(name) do
          default = options[:default]
          default.respond_to?(:call) ? instance_exec(&default) : default
        end
      end
      define_method(:"#{name}=") { |value| @_values[name] = value }
      name
    end

    def property!(name, **options)
      property(name, **options, required: true)
    end
  end

  def self.included(base)
    base.extend(ClassMethods)
  end

  def initialize(**attrs)
    @_values = attrs.dup
    self.class.properties.each do |name, options|
      raise MissingValueError.new(self, name) if options[:required] && !@_values.key?(name)
    end
  end
end
"##;

/// The parts of the gem's Tapioca RBI this fixture uses, as Tapioca
/// writes them for the real smart_properties 1.17.0.
const RBI: &str = r#"# typed: true
module SmartProperties
  mixes_in_class_methods ::SmartProperties::ClassMethods

  def initialize(**attrs); end

  class << self
    def included(base); end
  end
end
class SmartProperties::Error < ::ArgumentError; end
class SmartProperties::MissingValueError < ::SmartProperties::Error
  def initialize(owner, name); end
end
module SmartProperties::ClassMethods
  def properties; end
  def property(name, **options); end
  def property!(name, **options); end
end
SmartProperties::VERSION = T.let(T.unsafe(nil), String)
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    smart_properties (1.17.0)\n\nDEPENDENCIES\n  activesupport\n  smart_properties (= 1.17.0)\n";

/// A plain class including the gem module, with bare and `self.` calls.
const ENTITLEMENTS: &str = r#"class Entitlements
  include SmartProperties

  property! :plan, accepts: String
  property :seats, default: 3
  self.property :trial, default: false
end
"#;

/// A subclass of an app class that included the gem: the singleton
/// methods the mixin gave the superclass are inherited.
const OPTION: &str = r#"class PlanOption < Entitlements
  property :carrier, default: "ups"
end
"#;

/// A subclass of a plain app class that includes the gem itself.
const DESCRIBED: &str = r#"class Described
  def self.describe
    "described"
  end
end
"#;

const SHIPMENT: &str = r#"class Shipment < Described
  include ::SmartProperties

  property! :id
end
"#;

/// `extend` of the gem's class-methods module directly.
const CAPABILITIES: &str = r#"module Billing
  class Capabilities
    extend SmartProperties::ClassMethods

    property :flags, default: -> { [:base] }

    def initialize(**attrs)
      @_values = attrs
    end
  end
end
"#;

const SCRIPT: &str = r##"
e = Entitlements.new(plan: "plus")
puts [e.plan, e.seats, e.trial].inspect
puts Entitlements.properties.keys.inspect
begin
  Entitlements.new
rescue SmartProperties::MissingValueError => ex
  puts "raised: #{ex.message}"
end
o = PlanOption.new(plan: "basic", carrier: "dhl")
puts [o.plan, o.carrier, o.seats].inspect
puts PlanOption.properties.keys.inspect
s = Shipment.new(id: 7)
puts [s.id, Shipment.describe].inspect
c = Billing::Capabilities.new
puts c.flags.inspect
puts Billing::Capabilities.properties.keys.inspect
"##;

/// A gem home holding the fixture `smart_properties` 1.17.0, as `gem
/// install` lays one out.
fn gem_home(root: &Path) -> PathBuf {
    let home = root.join("gem-home");
    let gem = home.join("gems/smart_properties-1.17.0");
    std::fs::create_dir_all(gem.join("lib")).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(gem.join("lib/smart_properties.rb"), GEM_LIB).unwrap();
    std::fs::write(
        home.join("specifications/smart_properties-1.17.0.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"smart_properties\"\n  s.version = \"1.17.0\"\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/smart_properties.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    home
}

/// The fixture gem home ahead of the interpreter's own, so the emitted
/// tree's other gems (sqlite3, pg) still load.
fn gem_path(home: &Path) -> String {
    let out = emit_and_run::ruby().args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    format!("{}:{}", home.display(), String::from_utf8_lossy(&out.stdout))
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roundhouse-gem-mixin-macro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_loaded_gem_mixin_s_class_macros_run_in_the_class_body_as_in_cruby() {
    let root = scratch();
    let home = gem_home(&root);
    let gem_path = gem_path(&home);
    let sources = [ENTITLEMENTS, OPTION, DESCRIBED, SHIPMENT, CAPABILITIES].join("\n");
    let native = emit_and_run::ruby()
        .env("GEM_PATH", &gem_path)
        .arg("-e")
        .arg(format!("require \"smart_properties\"\n{sources}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(
        expected,
        [
            "[\"plus\", 3, false]",
            "[:plan, :seats, :trial]",
            "raised: Entitlements requires the property plan to be set",
            "[\"basic\", \"dhl\", 3]",
            "[:plan, :seats, :trial, :carrier]",
            "[7, \"described\"]",
            "[:base]",
            "[:flags]",
        ],
        "native control"
    );

    let (tree, errors) = emit_and_run::empty_app()
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/smart_properties@1.17.0.rbi", RBI)
        .write("app/models/entitlements.rb", ENTITLEMENTS)
        .write("app/models/plan_option.rb", OPTION)
        .write("app/models/described.rb", DESCRIBED)
        .write("app/models/shipment.rb", SHIPMENT)
        .write("app/models/billing/capabilities.rb", CAPABILITIES)
        .emit(BuildTarget::Ruby);
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby line for line\nerrors: {errors:#?}");
    assert!(errors.is_empty(), "{errors:#?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// What the gem's mixin does not provide stays dropped and reported: a
/// macro its class-methods module does not declare, and a `property`
/// call whose `include SmartProperties` names an app module of that name
/// (the namespace's own, which hides the gem's).
#[test]
fn a_macro_the_gem_mixin_does_not_declare_stays_dropped_and_reported() {
    const LOOSE: &str = "class Loose\n  include SmartProperties\n\n  property :kept\n  widget_macro :a\nend\n";
    const SHADOW: &str = "module Shadow\n  module SmartProperties\n    def self.marker\n      :app\n    end\n  end\n\n  class Thing\n    include SmartProperties\n\n    property :hidden\n  end\nend\n";
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("Gemfile.lock", LOCK),
        ("sorbet/rbi/gems/smart_properties@1.17.0.rbi", RBI),
        ("app/models/loose.rb", LOOSE),
        ("app/models/shadow.rb", SHADOW),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let ((app, files), diags) = roundhouse::emit::diagnostics::scope(|| {
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        roundhouse::session::analyze_and_lower(&mut app);
        let files = roundhouse::project::target_files(&app, Path::new("not-a-fixture"), BuildTarget::Ruby);
        (app, files)
    });
    let files = files.expect("ruby files");
    let gem = app.gem_boundary.classes.get(&roundhouse::ident::ClassId(roundhouse::ident::Symbol::from("SmartProperties")));
    assert_eq!(
        gem.map(|g| g.mixes_in_class_methods.iter().map(|m| m.0.as_str().to_string()).collect::<Vec<_>>()),
        Some(vec!["::SmartProperties::ClassMethods".to_string()]),
        "the RBI's mixes_in_class_methods is recorded"
    );
    let messages: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();
    let dropped = |name: &str, class: &str| {
        messages.iter().any(|m| m.contains(&format!("`{name}` in the body of `{class}` is not modelled and is dropped")))
    };
    assert!(dropped("widget_macro", "Loose"), "{messages:#?}");
    assert!(dropped("property", "Shadow::Thing"), "{messages:#?}");
    assert!(!dropped("property", "Loose"), "{messages:#?}");
    let loose = files
        .iter()
        .find(|(path, _)| path.ends_with("loose.rb"))
        .map(|(_, body)| body.clone())
        .unwrap_or_else(|| panic!("no loose.rb in {:?}", files.iter().map(|(p, _)| p).collect::<Vec<_>>()));
    assert!(loose.contains("property(:kept)") || loose.contains("property :kept"), "{loose}");
    assert!(!loose.contains("widget_macro"), "{loose}");
}
