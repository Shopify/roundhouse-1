//! A `def` in a Concern's `included do` is class_eval'd onto each
//! includer, so it sits above every module the block includes.
//!
//! Core's Apps::Extension: its block includes SmartProperties, then
//! defines `initialize(app_id:, **config)`, which calls `super(config)`.
//! Kept on the Concern, the def sat below SmartProperties, whose
//! `initialize` then ran first on `TestExtension.new(app_id: 1, key:
//! "k")` and raised ConstructorArgumentForwardingError. Its `unless
//! method_defined?(:to_h) define_method :to_h ...` asks the includer
//! and was refused. Each case is compared with native Rails (the pinned
//! oracle, tests/support/rails_oracle.rb).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const PROPS: &str = r##"module Props
  def initialize(attrs = {})
    raise ArgumentError, "forwarded #{attrs.keys.inspect}" unless attrs.keys.all? { |k| k == :key }
    @attrs = attrs
  end

  def describe = "props"
  def attrs = @attrs
end
"##;

const EXT: &str = r##"module Ext
  extend ActiveSupport::Concern

  included do
    include Props

    def initialize(app_id:, **config)
      @app_id = app_id
      super(config)
    end

    def describe = "ext/#{super}"

    unless method_defined?(:to_h)
      define_method :to_h do
        { app_id: @app_id, **attrs }
      end
    end

    def app_id = @app_id
  end
end
"##;

const TEST_EXT: &str = "class TestExt\n  include Ext\nend\n";
/// The includer's superclass already answers `to_h`, so the block's
/// `define_method` is skipped; its own later `describe` replaces the
/// block's, so its `super` reaches Props.
const HASHY: &str = "class Hashy\n  def to_h = { hashy: true }\nend\n";
const OWN_EXT: &str = "class OwnExt < Hashy\n  include Ext\n\n  def describe = \"own/#{super}\"\nend\n";

const SCRIPT: &str = r##"e = TestExt.new(app_id: 1, key: "k")
p [e.app_id, e.attrs, e.describe, e.to_h]
o = OwnExt.new(app_id: 2)
p [o.app_id, o.attrs, o.describe, o.to_h]
p [TestExt.instance_method(:initialize).owner, TestExt.instance_method(:to_h).owner, OwnExt.instance_method(:to_h).owner]
p [TestExt.private_method_defined?(:initialize), Ext.method_defined?(:app_id)]
begin
  TestExt.new(app_id: 1, other: 2)
rescue ArgumentError => error
  p error.message
end
"##;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

fn native(program: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rh_concern_included_block_ancestry_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.rb"),
        format!(
            "require {ORACLE:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\nrequire \"active_support/concern\"\n{program}"
        ),
    )
    .unwrap();
    let out = std::process::Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn included_block_defs_sit_above_the_blocks_includes_as_native() {
    let classes = [
        ("app/models/concerns/props.rb", PROPS),
        ("app/models/concerns/ext.rb", EXT),
        ("app/models/test_ext.rb", TEST_EXT),
        ("app/models/hashy.rb", HASHY),
        ("app/models/own_ext.rb", OWN_EXT),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native(&format!("{program}{SCRIPT}"));
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#"[1, {key: "k"}, "ext/props", {app_id: 1, key: "k"}]"#,
            r#"[2, {}, "own/props", {hashy: true}]"#,
            "[TestExt, TestExt, Hashy]",
            "[true, false]",
            "\"forwarded [:other]\"",
        ],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| overlay.write(path, source))
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from native line for line");
}
