//! `CurrentAttributes.set`, `attribute … default:` and an attribute
//! writer reaching the storage through a bare `super`, run against the
//! pinned activesupport 8.2 on the same source.
//!
//! The Core shapes behind each case:
//! * `Podding::Selector.select_pod` passes a Hash built at run time,
//!   `Current.set(pod_attributes(pod_id, options)) do … end`, and other
//!   helpers forward `&block` (`Current.set(shard_db_readonly: value,
//!   &block)`).
//! * `Podding::Current#pod_id=` clears `pod_name` and ends in a bare
//!   `super`.
//! * `ApplicationSection::Current` declares `attribute :section,
//!   default: -> { NilSectionGlobals }`, and `ApplicationSection.name` is
//!   `current.name`.

use roundhouse::det::HashMap;
use std::path::PathBuf;
use std::process::Command;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CURRENT: &str = r#"class Current < ActiveSupport::CurrentAttributes
  attribute :a
  attribute :b, default: -> { :dflt }
  attribute :c, :d, default: []
  attribute :log_seen
  attribute :counter, default: 0

  def a=(v)
    super(v.to_i)
  end

  def c=(value)
    self.log_seen = :c_written
    super
  end

  def d
    super || :never
  end

  def greet(name, punct: "!")
    "hi #{name}#{punct} #{b}"
  end

  private

  def secret
    :s
  end
end
"#;

const PODDING_CURRENT: &str = r#"module Podding
  class Current < ActiveSupport::CurrentAttributes
    attribute :pod_id, :pod_name

    def pod_id=(value)
      self.pod_name = nil
      super
    end
  end
end
"#;

const PROBE: &str = r#"class CurrentProbe
  def self.run_with(&blk)
    Current.set(b: :in, &blk)
  end

  def self.run_hash(h, &blk)
    Current.set(**h, &blk)
  end

  def self.early(v)
    Current.set(b: v) { return :returned }
    :not_reached
  end

  def self.run
    out = []
    out << Current.a.inspect << Current.b.inspect << Current.c.inspect << Current.counter.inspect
    h = { a: "1" }
    out << Current.set(h) { Current.a }.inspect
    out << Current.a.inspect
    Current.c << 1
    out << Current.c.inspect
    Current.reset
    out << Current.c.inspect << Current.c.equal?(Current.d).inspect << Current.b.inspect
    Current.c = [5]
    out << Current.log_seen.inspect << Current.c.inspect
    out << run_with { |cur| [cur.class.name, Current.b] }.inspect
    out << Current.b.inspect
    out << run_hash({ b: 2 }) { Current.b }.inspect
    out << Current.b.inspect
    out << Current.set(b: 9) { next 7 }.inspect
    out << [1, 2].map { |i| Current.set(b: i) { break :broke } }.inspect
    out << Current.b.inspect
    begin
      Current.set(b: :boom) { raise "x" }
    rescue => e
      out << [e.message, Current.b].inspect
    end
    out << early(:e).inspect << Current.b.inspect
    out << Current.instance.set(b: 3) { Current.b }.inspect
    out << Current.set(a: "4", b: :two) { [Current.a, Current.b] }.inspect
    out << [Current.a, Current.b].inspect
    out << Current.set("b" => :strkey) { Current.b }.inspect
    begin
      Current.set(zzz: 1) { :never }
    rescue NoMethodError => e
      out << e.message.inspect
    end
    out << Current.greet("x").inspect << Current.greet("y", punct: "?").inspect
    out << Current.respond_to?(:secret).inspect
    out << Current.d.inspect
    Current.d = :dd
    out << Current.set(d: nil) { Current.d }.inspect << Current.d.inspect
    k = Current.set(counter: 5) do
      Current.counter += 1
      Current.counter
    end
    out << k.inspect << Current.counter.inspect
    Podding::Current.pod_name = :orig
    out << ::Podding::Current.set(pod_name: :x, pod_id: 5) { [Podding::Current.pod_id, Podding::Current.pod_name] }.inspect
    out << [Podding::Current.pod_id, Podding::Current.pod_name].inspect
    out
  end
end
"#;

/// Every line the probe prints, from activesupport and from the emitted
/// tree, must agree; a few are pinned so a drifting oracle cannot make
/// both sides agree on something else.
#[test]
fn set_defaults_and_writer_super_run_like_activesupport() {
    let oracle = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/rails_oracle");
    let script = std::env::temp_dir().join(format!("rh_current_set_native_{}.rb", std::process::id()));
    std::fs::write(
        &script,
        format!(
            "require {:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\n\
             require \"active_support/current_attributes\"\n{CURRENT}\n{PODDING_CURRENT}\n{PROBE}\n\
             puts CurrentProbe.run",
            oracle.display().to_string()
        ),
    )
    .unwrap();
    let native = Command::new("ruby").arg(&script).output().expect("ruby");
    std::fs::remove_file(&script).unwrap();
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let native = String::from_utf8_lossy(&native.stdout).into_owned();
    let lines: Vec<&str> = native.lines().collect();
    // `Current.set(h) { Current.a }` with `h = { a: "1" }`: the writer's
    // `super(v.to_i)`, then the restore.
    assert_eq!(lines[..6], ["nil", ":dflt", "[]", "0", "1", "0"], "{native}");
    // A Proc default is called per reset, a value is `dup`ed per
    // attribute.
    assert_eq!(lines[7..10], ["[]", "false", ":dflt"], "{native}");
    assert!(native.contains("[:broke, :broke]\n"), "{native}");
    // The literal pairs in `with`'s order: `pod_id=` clears the
    // `pod_name` just written, and its restore clears it again.
    assert!(native.ends_with("[5, nil]\n[nil, nil]\n"), "{native}");

    let run = emit_and_run::real_blog()
        .write("app/models/current.rb", CURRENT)
        .write("app/models/podding/current.rb", PODDING_CURRENT)
        .write("app/models/current_probe.rb", PROBE)
        .run_ruby("puts CurrentProbe.run");
    run.assert_passes();
    assert_eq!(native, run.stdout);
}

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

fn emitted_current() -> String {
    let app = ingest_app_from_tree(tree(&[
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/current.rb", CURRENT),
    ]))
    .expect("ingest");
    ruby::emit_library(&app)
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("current.rb"))
        .map(|f| f.content.clone())
        .expect("current.rb")
}

/// A bare `super` in a writer is the storage write of its parameter.
#[test]
fn a_bare_super_in_a_writer_writes_its_parameter() {
    let c = emitted_current();
    assert!(!c.contains("super"), "{c}");
    assert!(c.contains("def c=(value)\n    self.log_seen = :c_written\n    @c = value"), "{c}");
    assert!(c.contains("def a=(v)\n    @a = v.to_i"), "{c}");
}

/// Defaults are evaluated by `initialize`, so on first use and on every
/// `reset`: a lambda's body runs, a mutable value is `dup`ed.
#[test]
fn defaults_are_evaluated_per_instance() {
    let c = emitted_current();
    assert!(c.contains("def initialize\n    @b = Current.__rh_default_b"), "{c}");
    assert!(c.contains("def self.__rh_default_b\n    :dflt"), "{c}");
    assert!(c.contains("def self.__rh_default_c\n    [].dup"), "{c}");
    assert!(c.contains("def self.__rh_default_counter\n    0"), "{c}");
    assert!(!c.contains("@a = Current.__rh_default"), "no default declared for a:\n{c}");
}

/// `set` exists on the instance and the class for any argument shape;
/// only public methods are forwarded to the class (Rails'
/// `method_added`), with their parameters.
#[test]
fn set_and_public_forwarders_are_generated() {
    let c = emitted_current();
    assert!(c.contains("def set(attributes)"), "{c}");
    assert!(c.contains("def self.set(attributes, &block)\n    Current.instance.set(attributes, &block)"), "{c}");
    // The keyword passes on as one; its default is written back as a
    // keyword in the full pipeline (`lower::keyword_marks`).
    assert!(c.contains("def self.greet(name, punct"), "{c}");
    assert!(c.contains("Current.instance.greet(name, punct: punct)"), "{c}");
    assert!(!c.contains("def self.secret"), "a private method is not forwarded:\n{c}");
}

/// An app-written `set` replaces the generated one, and is forwarded
/// once, with its own parameters.
#[test]
fn an_app_written_set_is_kept_and_forwarded_once() {
    let app = ingest_app_from_tree(tree(&[
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        (
            "app/models/current.rb",
            "class Current < ActiveSupport::CurrentAttributes\n  attribute :a\n\n  def set(value)\n    self.a = value\n  end\nend\n",
        ),
    ]))
    .expect("ingest");
    let c = ruby::emit_library(&app)
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("current.rb"))
        .map(|f| f.content.clone())
        .expect("current.rb");
    assert!(!c.contains("def set(attributes)"), "{c}");
    assert_eq!(c.matches("def self.set(").count(), 1, "{c}");
    assert!(c.contains("def self.set(value)\n    Current.instance.set(value)"), "{c}");
}
