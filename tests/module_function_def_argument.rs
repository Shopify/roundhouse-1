//! `module_function def name … end`: the argument form whose argument is
//! the definition itself (`def` returns its name). Ruby defines the
//! instance method, then copies it: the module gets a public singleton
//! method and the instance method becomes private, so an includer's bare
//! call and `Mod.name(...)` both work.
//!
//! Core's GraphApi::ContextBasedVisibility is nothing else. Ingest used
//! to see no direct `def` in it, so the module was never surfaced and
//! every includer (GraphApi::Argument) failed to load.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::dialect::{MethodDef, MethodReceiver, MethodVisibility};
use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_library_classes;
use roundhouse::App;
use std::process::Command;

fn methods(source: &str) -> Vec<MethodDef> {
    let classes = ingest_library_classes(source.as_bytes(), "visibility.rb").expect(source);
    let class = classes.into_iter().find(|c| c.name.0.as_str() == "Visibility").expect("Visibility surfaces");
    class.methods
}

fn visibility(methods: &[MethodDef], name: &str, receiver: MethodReceiver) -> MethodVisibility {
    methods
        .iter()
        .rev()
        .find(|m| m.name.as_str() == name && m.receiver == receiver)
        .unwrap_or_else(|| panic!("{name} {receiver:?} in {methods:#?}"))
        .visibility
}

fn emit(source: &str) -> String {
    let classes = ingest_library_classes(source.as_bytes(), "visibility.rb").expect("ingest test source");
    let mut app = App::new();
    app.library_classes.extend(classes);
    emit_library(&app)
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n")
}

const MODULE: &str = r##"module Visibility
  module_function def client(context)
    context ? context[:client] : "current"
  end

  module_function def label(context)
    "client=#{client(context)}"
  end
end
"##;

#[test]
fn a_module_of_module_function_defs_surfaces_with_both_copies() {
    let methods = methods(MODULE);
    for name in ["client", "label"] {
        assert_eq!(visibility(&methods, name, MethodReceiver::Class), MethodVisibility::Public, "{name}");
        assert_eq!(visibility(&methods, name, MethodReceiver::Instance), MethodVisibility::Private, "{name}");
    }
    assert_eq!(methods.len(), 4, "{methods:#?}");
}

#[test]
fn the_emitted_module_defines_the_public_copy_and_the_private_method() {
    let out = emit(MODULE);
    for name in ["client", "label"] {
        assert!(out.contains(&format!("def self.{name}(context)")), "{out}");
        assert!(out.contains(&format!("def {name}(context)")), "{out}");
        assert!(out.contains(&format!("private :{name}")), "{out}");
    }
}

#[test]
fn later_visibility_calls_address_their_own_side() {
    // `private_class_method` changes the copy; `public` the instance method.
    let source = "module Visibility\n  module_function def a\n    1\n  end\n  private_class_method :a\n  module_function def b\n    2\n  end\n  public :b\nend\n";
    let methods = methods(source);
    assert_eq!(visibility(&methods, "a", MethodReceiver::Class), MethodVisibility::Private);
    assert_eq!(visibility(&methods, "a", MethodReceiver::Instance), MethodVisibility::Private);
    assert_eq!(visibility(&methods, "b", MethodReceiver::Class), MethodVisibility::Public);
    assert_eq!(visibility(&methods, "b", MethodReceiver::Instance), MethodVisibility::Public);
}

#[test]
fn unmodeled_placements_are_refused_by_name() {
    // Ruby has no `module_function` on a singleton class (NoMethodError).
    let singleton = "module Visibility\n  class << self\n    module_function def a\n      1\n    end\n  end\nend\n";
    let error = ingest_library_classes(singleton.as_bytes(), "visibility.rb").err().expect("refused");
    assert!(format!("{error}").contains("module_function on a nested singleton level"), "{error}");
    // A definition that only sometimes runs is refused, not dropped: the
    // module surfaces so the refusal is reported.
    let conditional = "module Visibility\n  if ENV[\"READY\"]\n    module_function def a\n      1\n    end\n  end\nend\n";
    let error = ingest_library_classes(conditional.as_bytes(), "visibility.rb").err().expect("refused");
    assert!(format!("{error}").contains("conditional definition under `module_function`"), "{error}");
}

const INCLUDER: &str = r##"class Reader
  include Visibility

  def read(context)
    [client(context), label(context)]
  end
end
"##;

/// One observation per line; an exception shows as its class.
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    e.class.name
  end
  puts "#{label}: #{value.inspect}"
end
seen("module call") { Visibility.client({ client: "app" }) }
seen("module call nil") { Visibility.label(nil) }
seen("includer") { Reader.new.read({ client: "x" }) }
seen("public instance call") { Reader.new.client(nil) }
seen("singleton methods") { Visibility.singleton_methods.sort }
seen("private instance methods") { Visibility.private_instance_methods(false).sort }
seen("public instance methods") { Visibility.public_instance_methods(false).sort }
"##;

#[test]
fn module_and_includer_calls_answer_like_ruby() {
    let emitted = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("lib/visibility.rb", MODULE)
        .write("lib/reader.rb", INCLUDER)
        .run_ruby(SCRIPT);
    emitted.assert_passes();
    let dir = std::env::temp_dir().join(format!("rh_module_function_def_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("native.rb"), format!("{MODULE}{INCLUDER}{SCRIPT}")).unwrap();
    let native = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let native = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(emitted.stdout, native, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    for expected in [
        "module call: \"app\"",
        "module call nil: \"client=current\"",
        "includer: [\"x\", \"client=x\"]",
        "public instance call: \"NoMethodError\"",
        "singleton methods: [:client, :label]",
        "private instance methods: [:client, :label]",
        "public instance methods: []",
    ] {
        assert!(native.lines().any(|l| l == expected), "missing {expected:?} in\n{native}");
    }
}
