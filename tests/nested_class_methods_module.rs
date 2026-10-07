//! A nested `module ClassMethods` is a module of its own.
//!
//! Ingest used to fold its defs into the owner as `def self.x` and drop
//! the module, so `Owner.respond_to?(:x)` answered true, the constant was
//! undefined, and every other read of it (`k.is_a?(ClassMethods)`,
//! `extend(ClassMethods)` in an instance method, `#: as ClassMethods`)
//! was refused at check. Core's `LiquidDropActsAsString`,
//! `ToggleFlagsHelper`, `ActiveSupport::Testing::Let` and graphql-cardinal's
//! `TransformBreadthVerifierObjects` are these shapes.
//!
//! The module is emitted as written, the authored `included` hook extends
//! the includer, and an `ActiveSupport::Concern` includer (whose
//! `append_features` the emitted module no longer has) extends each
//! carrier itself, in Ruby's order. The fold survives only as typing:
//! the carrier's defs are typed as the includer's class side, with
//! `self` a class object (`define_method`, `self < Object`). A
//! module-level `class << self` is the module's own singleton and reaches
//! no includer, Concern or not. Native Ruby on the same sources is the
//! oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

/// Files in load order (the native control requires them in this order).
const SOURCES: &[(&str, &str)] = &[
    ("flaggy", r#"module Flaggy
  PREFIX = "flaggy"

  def self.included(base)
    base.extend(ClassMethods)
  end

  module ClassMethods
    def flag = :flag

    def prefix(value = PREFIX) = value

    def flag_alias(name)
      define_method(name) { :defined }
    end

    def object_subclass? = self < Object
  end
end
"#),
    ("user_thing", "class UserThing\n  include Flaggy\n  PREFIX = \"local\"\nend\n"),
    ("letty", r#"module Letty
  extend ActiveSupport::Concern

  class << self
    def own_shift = :letty
  end

  module ClassMethods
    def lets = { a: 1 }

    def parent_lets(k)
      k.is_a?(ClassMethods) ? k.lets : :none
    end
  end
end
"#),
    ("let_user", "class LetUser\n  include Letty\nend\n"),
    ("let_child", "class LetChild < LetUser\nend\n"),
    ("transformy", r#"module Transformy
  class << self
    def included(base)
      base.extend(ClassMethods)
    end
  end

  module ClassMethods
    def transform
      :class_transform
    end
  end

  def transform
    self.class #: as Transformy::ClassMethods
      .transform
  end
end
"#),
    ("transformed", "class Transformed\n  include Transformy\nend\n"),
    ("acts", r#"module Acts
  def acts_as_thing
    extend(ClassMethods)
    thing_names
  end

  module ClassMethods
    def thing_names = [:thing]
  end
end
"#),
    ("acts_host", "class ActsHost\n  include Acts\nend\n"),
    ("inner_concern", r#"module InnerConcern
  extend ActiveSupport::Concern

  module ClassMethods
    def inner = :inner
    def who = :inner
  end
end
"#),
    ("outer_concern", r#"module OuterConcern
  extend ActiveSupport::Concern
  include InnerConcern

  module ClassMethods
    def outer = :outer
    def who = :outer
  end
end
"#),
    ("concern_host", "class ConcernHost\n  include OuterConcern\nend\n"),
];

const REPORT: &str = r#"h = ActsHost.new
UserThing.flag_alias(:fa)
p [UserThing.flag, Flaggy.respond_to?(:flag), Flaggy.const_defined?(:ClassMethods),
   UserThing.singleton_class.include?(Flaggy::ClassMethods), Flaggy::ClassMethods.instance_methods.sort,
   UserThing.new.fa, UserThing.object_subclass?, UserThing.prefix]
p [LetUser.lets, LetChild.parent_lets(LetChild), LetUser.parent_lets(Object), Letty.respond_to?(:lets),
   Letty.own_shift, LetUser.respond_to?(:own_shift)]
p [Transformed.new.transform, Transformy.respond_to?(:transform), Transformed.respond_to?(:included)]
p [h.acts_as_thing, h.singleton_class.include?(Acts::ClassMethods), ActsHost.respond_to?(:thing_names)]
p [ConcernHost.inner, ConcernHost.outer, ConcernHost.who,
   ConcernHost.singleton_class.ancestors.map(&:name).compact.grep(/Concern::ClassMethods/)]
"#;

const EXPECTED: &str = r#"[:flag, false, true, true, [:flag, :flag_alias, :object_subclass?, :prefix], :defined, true, "flaggy"]
[{a: 1}, {a: 1}, :none, false, :letty, false]
[:class_transform, false, false]
[[:thing], true, false]
[:inner, :outer, :outer, ["OuterConcern::ClassMethods", "InnerConcern::ClassMethods"]]
"#;

fn native() -> String {
    let dir = std::env::temp_dir().join(format!("roundhouse-nested-class-methods-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut script = String::from("require \"active_support\"\nrequire \"active_support/concern\"\n");
    for (name, text) in SOURCES {
        let file = dir.join(format!("{name}.rb"));
        std::fs::write(&file, text).unwrap();
        script.push_str(&format!("require {:?}\n", file.display().to_string()));
    }
    script.push_str(REPORT);
    let output = std::process::Command::new("ruby").arg("-e").arg(&script).output().expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "native control (needs activesupport): {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn nested_class_methods_module_runs_as_in_native_ruby() {
    let expected = native();
    assert_eq!(expected, EXPECTED, "native control");

    let mut app = emit_and_run::empty_app()
        .write("db/schema.rb", "ActiveRecord::Schema.define do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    for (name, text) in SOURCES {
        app = app.write(&format!("app/lib/{name}.rb"), text);
    }
    let (emitted, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));

    // The carrier is its own file and module; the owner keeps its hook
    // and gains no class-side copy; the includer is not given one either.
    let flaggy = std::fs::read_to_string(emitted.join("app/models/flaggy.rb")).unwrap();
    assert!(flaggy.contains("base.extend(Flaggy::ClassMethods)"), "{flaggy}");
    assert!(!flaggy.contains("def self.flag"), "{flaggy}");
    let carrier = std::fs::read_to_string(emitted.join("app/models/flaggy/class_methods.rb")).unwrap();
    assert!(carrier.contains("module ClassMethods") && carrier.contains("def flag"), "{carrier}");
    let user_thing = std::fs::read_to_string(emitted.join("app/models/user_thing.rb")).unwrap();
    assert!(!user_thing.contains("def self.flag") && !user_thing.contains("extend"), "{user_thing}");
    let host = std::fs::read_to_string(emitted.join("app/models/concern_host.rb")).unwrap();
    let (inner, outer) = (host.find("extend InnerConcern::ClassMethods"), host.find("extend OuterConcern::ClassMethods"));
    assert!(inner.is_some() && outer.is_some() && inner < outer, "{host}");

    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"./boot\"\n{REPORT}"))
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// The compiled targets have no `extend`: there the includer keeps the
/// carrier's defs as its own class methods (the pre-existing reading),
/// and a hook whose whole body is the bridge is dropped rather than left
/// to raise at include time.
#[test]
fn spinel_keeps_the_class_side_copies_and_drops_the_bridge() {
    let mut app = emit_and_run::empty_app()
        .write("db/schema.rb", "ActiveRecord::Schema.define do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    for (name, text) in SOURCES {
        app = app.write(&format!("app/lib/{name}.rb"), text);
    }
    let (emitted, errors) = app.emit(BuildTarget::Spinel);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let read = |path: &str| std::fs::read_to_string(emitted.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let flaggy = read("app/models/flaggy.rb");
    assert!(!flaggy.contains("def self.included") && !flaggy.contains("raise"), "{flaggy}");
    let transformy = read("app/models/transformy.rb");
    assert!(!transformy.contains("included") && !transformy.contains("raise"), "{transformy}");
    let user_thing = read("app/models/user_thing.rb");
    assert!(user_thing.contains("def self.flag") && user_thing.contains("def self.prefix"), "{user_thing}");
    let host = read("app/models/concern_host.rb");
    assert!(host.contains("def self.inner") && host.contains("def self.outer") && !host.contains("extend"), "{host}");
}
