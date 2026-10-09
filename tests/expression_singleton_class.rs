//! `class << self` at expression position defines on the singleton class.
//!
//! Core's Apps::Enum builds each enum with `Class.new do ... class << self
//! ... alias_method :[], :enum_for; private; attr_reader :enums; def
//! register_enum ... end end`. Inside the block no class-body walk
//! classifies the `class << self`, and it used to evaluate to its last
//! statement with every macro sent to the object: `alias_method` defined
//! an instance method, so `Types[:x]` raised `undefined method '[]' for
//! class Apps::Models::Notifications::TypesV2::Types`.
//!
//! Each statement is now written against `singleton_class`: `def x` as
//! `def self.x`, `attr_*`, `alias_method` / `alias`, `define_method` and
//! the other macros as `singleton_class.send(:name, ...)`, `private
//! :new` and a bare `private` as the singleton methods' visibility.
//! What it cannot write is refused by name. The probes run natively and
//! emitted, and must print the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use roundhouse::ingest::{ingest_app_from_tree, survey};
use roundhouse::project::BuildTarget;
use std::path::PathBuf;
use std::process::Command;

/// The task's shape at file level, with `fetch` defined before its alias
/// as Ruby requires.
const KLASS: &str = r##"Klass = Class.new do
  class << self
    def fetch(key)
      @all ||= {}
      @all[key] ||= "v#{key}"
    end

    attr_reader :all
    alias_method :[], :fetch
    alias lookup fetch
    private :new

    private

    attr_writer :hidden
    define_method(:secret) { :s }

    def helper
      :h
    end

    public

    def visible = :v
  end
end
"##;

/// Apps::Enum's shape: a module method returning `Class.new do ... end`.
const ENUM: &str = r##"module Enum
  class << self
    def new(*values)
      values = values.map(&:to_sym).freeze

      Class.new do
        include Enum

        define_singleton_method(:new, Object.method(:new))
        define_singleton_method(:values) { values }

        class << self
          def all
            enums.values
          end

          def enum_for(value)
            enums.fetch(value)
          end

          alias_method :[], :enum_for

          private

          attr_reader :enums

          def register_enum(value)
            @enums ||= {}
            @enums[value] ||= new(value)
          end
        end

        def initialize(value)
          raise ArgumentError unless self.class.values.include?(value)

          @value = value
        end

        def to_s
          @value.to_s
        end

        values.each do |reference_value|
          define_singleton_method(reference_value) { enum_for(reference_value) } unless reference_value == :new
          define_method(:"#{reference_value}?") { reference_value == @value }
          send(:register_enum, reference_value)
        end
      end
    end
  end
end
"##;

const STATES: &str = "States = Enum.new(:new_state, :dismissed, :hidden)\n";

const OBSERVE: &str = r##"p [Klass[:a], Klass.lookup(:b), Klass.all, Klass.visible, Klass.respond_to?(:new), Klass.respond_to?(:hidden=), Klass.respond_to?(:secret), Klass.send(:secret), Klass.respond_to?(:helper), Klass.send(:helper)]
p Klass.singleton_class.private_instance_methods(false).sort
begin
  Klass.new
rescue NoMethodError => e
  p e.message[/private method '[^']+'/]
end
p [States.all.map(&:to_s), States[:hidden].to_s, States.hidden.hidden?, States.dismissed == States[:dismissed], States.respond_to?(:enums), States.respond_to?(:register_enum)]
p States.singleton_class.private_instance_methods(false).sort
begin
  States[:nope]
rescue KeyError => e
  p e.class
end
"##;

const FILES: &[(&str, &str)] = &[
    ("app/models/klass.rb", KLASS),
    ("app/models/enum.rb", ENUM),
    ("app/models/states.rb", STATES),
];

fn requires() -> String {
    FILES
        .iter()
        .map(|(path, _)| format!("require_relative {:?}\n", path.trim_end_matches(".rb")))
        .collect()
}

fn native() -> String {
    let dir = std::env::temp_dir().join(format!("rh_expression_sclass_native_{}", std::process::id()));
    for (path, text) in FILES {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let out = Command::new("ruby")
        .args(["-W0", "-e", &format!("{}{OBSERVE}", requires())])
        .current_dir(&dir)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn class_new_singleton_blocks_define_on_the_singleton_class_like_ruby() {
    let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
    for (path, text) in FILES {
        app = app.write(path, text);
    }
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let expected = native();
    assert_eq!(
        expected,
        r#"["va", "vb", {a: "va", b: "vb"}, :v, false, false, false, :s, false, :h]
[:helper, :hidden=, :new, :secret]
"private method 'new'"
[["new_state", "dismissed", "hidden"], "hidden", true, true, false, false]
[:enums, :register_enum]
KeyError
"#,
        "native control"
    );
    // A file-level `Klass = Class.new do` is written as a class (with its
    // `class << self` as a class body's); Enum's, inside a method, is the
    // expression-position form.
    let enum_rb = std::fs::read_to_string(dir.join("app/models/enum.rb")).unwrap();
    for written in [
        "def self.enum_for(value)",
        "singleton_class.send(:alias_method, :[], :enum_for)",
        "singleton_class.send(:private, singleton_class.send(:attr_reader, :enums))",
        "singleton_class.send(:private, :register_enum)",
    ] {
        assert!(enum_rb.contains(written), "`{written}` not written:\n{enum_rb}");
    }
    let script = format!("require File.expand_path('main', Dir.pwd)\n{}{OBSERVE}", requires());
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "emitted run failed in {}:\n{stdout}\n{}\n{enum_rb}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(stdout, expected, "emitted vs native");
}

/// Survey-mode ingest of `files` and every gap recorded on the way.
fn gaps(files: &[(&str, &str)]) -> Vec<String> {
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .chain([(PathBuf::from("roundhouse.yml"), b"test_paths:\n  - test\n".to_vec())])
        .collect();
    survey::activate();
    ingest_app_from_tree(tree).expect("ingest");
    survey::drain().iter().map(|g| format!("{g:?}")).collect()
}

/// A statement the rewrite cannot write against the singleton class is
/// refused by name, one statement at a time; the others still ingest.
#[test]
fn unmodeled_singleton_statements_are_refused_by_name() {
    let source = r##"module Refused
  def self.build
    Class.new do
      class << self
        LIMIT = 3
        memoize(:x) { 1 }
        attr_reader(*column_names)
        define_method(:y) { helper }
        module_function
        @count = 0
        def ok = :ok
      end

      class << Object.new
        def z; end
      end
    end
  end
end
"##;
    let gaps = gaps(&[("app/models/refused.rb", source)]);
    for why in [
        "a constant assigned in the body is not modeled",
        "`memoize` with a block is not modeled",
        "`attr_reader` with an argument that reads the singleton scope's `self` is not modeled",
        "`define_method` whose block calls the bare name `helper` is not modeled",
        "`module_function` (undefined on a singleton class) is not modeled",
        "a statement reading `self`, an instance variable, `yield` or `super` of the singleton scope is not modeled",
        "only `class << self` is modeled, not `class << <expr>`",
    ] {
        let message = format!("`class << self` at expression position: {why}");
        assert!(gaps.iter().any(|g| g.contains(&message)), "missing {message:?} in {gaps:#?}");
    }
    assert_eq!(gaps.len(), 7, "{gaps:#?}");
}
