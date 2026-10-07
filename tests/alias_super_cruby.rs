//! `alias_method` / `alias` of a method the class defines, where the
//! original calls `super`.
//!
//! Ruby looks an aliased method's `super` up under the ORIGINAL name:
//! shopify-money's `Money::Currency` has `class << self; def new(iso) …
//! super(iso) …; alias_method :find!, :new`, and `Currency.find!("usd")`
//! reaches `Class#new`. Ingest copies the original's def under the new
//! name. Emitted as `def self.find!`, that copy's `super` looked up
//! `Class#find!` and raised NoMethodError. The Ruby family writes the
//! native alias instead. Each probe prints what it observes, and the
//! emitted tree must print the same lines, in the same order, as CRuby
//! running the authored source directly.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SOURCE: &str = r#"class AliasRegistry
  @@loaded = {}

  class << self
    def new(key)
      @@loaded[key] ||= super(key)
    end
    alias_method :find!, :new

    def build(key)
      "built:" + key
    end
    alias fetch build
  end

  attr_reader :key

  def initialize(key)
    @key = key
  end
end

class AliasBase
  def describe(tag = "base")
    "AliasBase#describe(#{tag})"
  end

  def label
    "AliasBase#label"
  end
end

class AliasChild < AliasBase
  def describe(tag = "child")
    "AliasChild>" + super
  end
  alias_method :explain, :describe

  def label
    "AliasChild>" + super()
  end
  alias title label

  def secret
    "secret"
  end
  private :secret

  private

  # An alias takes the original's visibility, not the section's.
  alias_method :open_label, :label
  alias_method :hidden, :secret

  public

  def version
    "v1"
  end
  alias_method :first_version, :version

  # A later def of the original does not reach the alias.
  def version
    "v2"
  end
end
"#;

const SCRIPT: &str = r#"
first = AliasRegistry.find!("usd")
puts "find! reaches Class#new: #{first.class} key=#{first.key}"
puts "find! and new share the memo: #{AliasRegistry.new("usd").equal?(first)}"
puts "alias keyword on the class side: #{AliasRegistry.fetch("eur")}"

child = AliasChild.new
puts "instance alias super (zsuper): #{child.explain}"
puts "instance alias super with args: #{child.explain("given")}"
puts "alias keyword super(): #{child.title}"
puts "alias under private keeps public: #{AliasChild.public_method_defined?(:open_label)} #{child.open_label}"
puts "alias of private stays private: #{AliasChild.private_method_defined?(:hidden)}"
puts "alias keeps the earlier def: #{child.first_version} #{child.version}"
begin
  child.hidden
  puts "private alias callable"
rescue NoMethodError => e
  puts "private alias refused: #{e.class}"
end
"#;

/// Every emitted Ruby file that declares one of the probe's classes
/// (the tree may split a source file per class), concatenated.
fn emitted_probe(root: &std::path::Path) -> String {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rb") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if ["class AliasRegistry", "class AliasChild"].iter().any(|c| text.contains(c)) {
                    out.push(text);
                }
            }
        }
    }
    let mut files = Vec::new();
    walk(root, &mut files);
    assert!(!files.is_empty(), "no emitted probe class under {}", root.display());
    files.join("\n")
}

/// The authored source, run by CRuby itself.
fn native_lines() -> Vec<String> {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{SOURCE}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect()
}

#[test]
fn aliases_calling_super_behave_as_in_cruby_after_emission() {
    let expected = native_lines();
    // The control must exercise each behaviour, or matching it proves nothing.
    for probe in [
        "find! reaches Class#new: AliasRegistry key=usd",
        "find! and new share the memo: true",
        "instance alias super (zsuper): AliasChild>AliasBase#describe(child)",
        "instance alias super with args: AliasChild>AliasBase#describe(given)",
        "alias keyword super(): AliasChild>AliasBase#label",
        "alias under private keeps public: true AliasChild>AliasBase#label",
        "alias of private stays private: true",
        "alias keeps the earlier def: v1 v2",
        "private alias refused: NoMethodError",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }

    let (tree, errors) = emit_and_run::real_blog()
        .write("app/services/alias_probe.rb", SOURCE)
        .emit(BuildTarget::Ruby);
    assert!(
        !errors.iter().any(|e| e.contains("alias_method copy calling super")),
        "every alias here has its original just before it: {errors:#?}"
    );
    let emitted = emitted_probe(&tree);
    for native in ["alias_method :find!, :new", "alias_method :fetch, :build", "alias_method :explain, :describe"] {
        assert!(emitted.contains(native), "expected the native alias {native:?}:\n{emitted}");
    }
    assert!(!emitted.contains("def self.find!"), "the alias must not be a copied def:\n{emitted}");

    let run = emit_and_run::real_blog()
        .write("app/services/alias_probe.rb", SOURCE)
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from CRuby line for line");
}

/// A target that emits the copy as a method of its own cannot keep the
/// original's name for `super`, so it refuses that copy by name.
#[test]
fn alias_copies_calling_super_are_refused_off_the_ruby_family() {
    let (_tree, errors) = emit_and_run::real_blog()
        .write("app/services/alias_probe.rb", SOURCE)
        .emit(BuildTarget::Spinel);
    assert!(
        errors.iter().any(|e| e.contains("alias_method copy calling super")),
        "spinel must refuse the copy that calls super: {errors:#?}"
    );
}
