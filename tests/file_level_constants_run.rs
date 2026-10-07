//! Constants a source file writes at FILE level, outside every class
//! and module, on the CRuby tree, against the same files run natively.
//!
//! Ruby binds those on Object, so a bare read finds them from any class
//! in any file. Ingest carries them on the file's first class (analysis
//! and lowering need an owner) and names them in its
//! `top_level_constants`; the Ruby emitter writes them at the top level
//! of that class's file. Before, they were written INSIDE the first
//! class, as `Outer::Inner::LIMIT`, and every reader outside it raised
//! NameError at run time while the check reported nothing (the analyzer
//! resolves a bare constant by name).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

/// Ahead of the first declaration, read from a sibling class of the
/// carrier, from inside the carrier, and alongside a module constant.
const SHAPES: &str = r##"LIMIT = 3

module Outer
  SCALE = 10

  class Inner
    def scaled
      SCALE * LIMIT
    end
  end

  class Other
    def limit
      LIMIT
    end
  end
end
"##;

/// After the declaration, reading the class it declares.
const BOX: &str = r##"class Box
  def self.kind
    "box"
  end
end

DEFAULT_KIND = Box.kind + "-default"
"##;

/// The carrier holds a nested class, which shares its file.
const SHELL: &str = r##"PREFIX = "sh"

class Shell
  class Core
    def tag
      "#{PREFIX}-core"
    end
  end

  def tag
    PREFIX
  end
end
"##;

/// No class or module declaration: the factory class carries it.
const POINT: &str = r##"ORIGIN_OFFSET = 1

Point = Struct.new(:x) do
  def shifted
    x + ORIGIN_OFFSET
  end
end
"##;

/// Another file, reading every one of them bare.
const READER: &str = r##"class Reader
  def read
    [LIMIT, Outer::SCALE, DEFAULT_KIND, PREFIX, ORIGIN_OFFSET]
  end
end
"##;

const FILES: [(&str, &str); 5] = [
    ("lib/shapes.rb", SHAPES),
    ("lib/box.rb", BOX),
    ("lib/shell.rb", SHELL),
    ("lib/point.rb", POINT),
    ("lib/reader.rb", READER),
];

const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message]
  end
  puts "#{label}: #{value.inspect}"
end
seen("inner") { Outer::Inner.new.scaled }
seen("other") { Outer::Other.new.limit }
seen("reader") { Reader.new.read }
seen("box") { DEFAULT_KIND }
seen("shell") { [Shell.new.tag, Shell::Core.new.tag] }
seen("point") { Point.new(4).shifted }
seen("owner") { [Object.const_defined?(:LIMIT, false), Outer::Inner.const_defined?(:LIMIT, false)] }
"##;

const EXPECTED: [&str; 7] = [
    "inner: 30",
    "other: 3",
    "reader: [3, 10, \"box-default\", \"sh\", 1]",
    "box: \"box-default\"",
    "shell: [\"sh\", \"sh-core\"]",
    "point: 5",
    "owner: [true, false]",
];

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n");
    for (path, source) in FILES {
        app = app.write(path, source);
    }
    app
}

/// The same files on plain Ruby, required in order.
fn native() -> String {
    let dir = std::env::temp_dir().join(format!("rh_file_constants_native_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    let mut program = String::new();
    for (path, source) in FILES {
        std::fs::write(dir.join(path), source).unwrap();
        program.push_str(&format!("require_relative {:?}\n", path.trim_end_matches(".rb")));
    }
    program.push_str(SCRIPT);
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn file_level_constants_live_on_object_like_the_native_run() {
    let native = native();
    assert_eq!(native.lines().collect::<Vec<_>>(), EXPECTED, "native:\n{native}");
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    assert_eq!(
        emitted.stdout.lines().collect::<Vec<_>>(),
        EXPECTED,
        "emitted:\n{}",
        emitted.stdout
    );
}
