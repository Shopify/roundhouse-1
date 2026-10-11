//! A class body mixes a module in through a local bound to `self`.
//!
//! Ruby's `Module#include`, `extend` and `prepend` are public, so a body
//! that holds its receiver in a local to dodge a static-typing cast
//! (`unsafe_self = self #: as untyped; unsafe_self.include(Mixins.build)`)
//! runs the mixin in the class body, and a module an expression builds is
//! loaded when it runs. The emit dropped these as "<expression>.include
//! not supported: model class-body call ... dropped", leaving the class
//! without the methods the module defines. The emitted tree must print
//! what CRuby prints, line for line.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const MIXINS: &str = r##"module Mixins
  def self.labelled(label)
    Module.new do
      define_method(:label) { label }
    end
  end

  def self.shouting
    Module.new do
      def shout
        "#{label.upcase}!"
      end
    end
  end

  def self.loud(prefix)
    Module.new do
      define_method(:describe) { "#{prefix} #{super()}" }
    end
  end
end
"##;

/// An alias under a cast, then a call-expression argument.
const WIDGET: &str = r##"class Widget
  include ActiveModel::Validations
  unsafe_self = self #: as untyped
  unsafe_self.include(Mixins.labelled("widget"))

  def describe
    "a #{label}"
  end
end
"##;

/// The receiver cast written inline, the call on the next line; the
/// module built by one call is included before one that reads it.
const GADGET: &str = r##"class Gadget
  include ActiveModel::Validations
  self #: as untyped
    .include(Mixins.labelled("gadget"))
  include(Mixins.shouting)
end
"##;

/// `prepend` through an alias wraps the class's own method.
const GIZMO: &str = r##"class Gizmo
  include ActiveModel::Validations
  alias_self = self #: as untyped
  alias_self.prepend(Mixins.loud("loudly"))

  def describe
    "gizmo"
  end
end
"##;

const SCRIPT: &str = r##"
w = Widget.new
puts [w.label, w.describe].inspect
g = Gadget.new
puts [g.label, g.shout].inspect
puts Gizmo.new.describe
"##;

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_mixin_through_a_local_bound_to_self_runs_in_the_class_body_as_in_cruby() {
    let sources = [MIXINS, WIDGET, GADGET, GIZMO].join("\n");
    let native = emit_and_run::ruby().arg("-e").arg(format!("require \"active_model\"\n{sources}\n{SCRIPT}")).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(
        expected,
        ["[\"widget\", \"a widget\"]", "[\"gadget\", \"GADGET!\"]", "loudly gizmo"],
        "native control"
    );

    let (tree, errors) = emit_and_run::empty_app()
        .write("lib/mixins.rb", MIXINS)
        .write("app/models/widget.rb", WIDGET)
        .write("app/models/gadget.rb", GADGET)
        .write("app/models/gizmo.rb", GIZMO)
        .emit(BuildTarget::Ruby);
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby line for line\nerrors: {errors:#?}");
    let dropped: Vec<_> = errors.iter().filter(|e| format!("{e:?}").contains("class-body call")).collect();
    assert!(dropped.is_empty(), "{dropped:#?}");
}
