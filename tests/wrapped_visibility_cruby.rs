//! A visibility marker around a declaration: `protected attr_writer(:split)`
//! (shopify-money's `Money::Splitter`), `private attr_reader :a, :b`,
//! `private alias_method :a, :b`. Ruby defines the methods with the
//! visibility in effect, and the marker then changes theirs: `attr_*`
//! returns the generated names, and `alias_method` returns the new one.
//! Before this, ingest refused the whole file ("visibility method names
//! must be literal symbols or strings"). The emitted tree must print the
//! same lines, in the same order, as CRuby running the authored source.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::dialect::{MethodReceiver, MethodVisibility};

const SOURCE: &str = r#"class WrapSplitter
  def initialize(value)
    @value = value
    @split = nil
  end

  protected attr_writer(:split)
  private attr_reader :value
  private attr_accessor :left, :right

  def split
    @split ||= [value, value]
  end

  def copy_into(other)
    other.split = split
    other
  end

  def total
    self.left = 1
    self.right = 2
    left + right
  end

  def shown
    "shown"
  end
  private alias_method :hidden_shown, :shown

  def after
    "after is public"
  end
end
"#;

const SCRIPT: &str = r##"
def visibility(klass, name)
  if klass.public_method_defined?(name) then "public"
  elsif klass.protected_method_defined?(name) then "protected"
  elsif klass.private_method_defined?(name) then "private"
  else "undefined"
  end
end

%i[split= value left left= right= shown hidden_shown after split].each do |name|
  puts "#{name}: #{visibility(WrapSplitter, name)}"
end
a = WrapSplitter.new(3)
b = WrapSplitter.new(9)
puts "protected writer from a peer: #{a.copy_into(b).split.inspect}"
puts "private accessors through self: #{a.total}"
begin
  a.split = [1]
  puts "protected writer callable from outside"
rescue NoMethodError => e
  puts "protected writer refused: #{e.class}"
end
puts "public after the markers: #{a.after}"
"##;

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
fn wrapped_declarations_take_the_marker_visibility_as_in_cruby() {
    let expected = native_lines();
    for probe in [
        "split=: protected",
        "value: private",
        "left=: private",
        "hidden_shown: private",
        "shown: public",
        "after: public",
        "protected writer from a peer: [3, 3]",
        "protected writer refused: NoMethodError",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }

    let run = emit_and_run::real_blog()
        .write("app/services/wrap_splitter.rb", SOURCE)
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from CRuby line for line");
}

/// A model keeps its accessors as items the lowering turns into methods,
/// so the marker's visibility rides on an override of each one.
#[test]
fn a_model_wrapped_accessor_carries_the_marker_visibility() {
    let model = "class WrapProbe < ApplicationRecord\n  protected attr_writer(:split)\n  private attr_reader :token\n\n  def shown\n    \"x\"\n  end\nend\n";
    let ingested = roundhouse::ingest::ingest_model(
        model.as_bytes(),
        "app/models/wrap_probe.rb",
        &roundhouse::schema::Schema::default(),
        &Default::default(),
    )
    .expect("a wrapped accessor ingests")
    .expect("model");
    let of = |name: &str| {
        ingested
            .visibility_overrides
            .iter()
            .find(|o| o.name.as_str() == name && o.receiver == MethodReceiver::Instance)
            .map(|o| o.visibility)
    };
    assert_eq!(of("split="), Some(MethodVisibility::Protected), "{:?}", ingested.visibility_overrides);
    assert_eq!(of("token"), Some(MethodVisibility::Private), "{:?}", ingested.visibility_overrides);
    let shown = ingested.methods().find(|m| m.name.as_str() == "shown").expect("shown");
    assert_eq!(shown.visibility, MethodVisibility::Public, "the markers wrap one declaration each");
}
