//! App files that hold one constant and no class or module declaration,
//! on the CRuby tree, against the same files run natively.
//!
//! Two shapes, both common in Shopify Core's app/models and app/jobs:
//! a class alias kept for old references (`RemoteDomain =
//! DomainSubscription`, `SyncFlowAutomationsJob = SyncFlowAutomationsJobV2`)
//! and a block-less factory class (`ApiClientVisibilityParent =
//! Struct.new(:api_client, :match_results)`). Neither file was carried
//! before, so every read of the constant was refused as an unsupported
//! constant. Rails' autoloader maps each file to its constant by name,
//! so the emitted `app/models.rb` must register each one under it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const THING: &str = r##"class Thing
  def self.kind
    "thing"
  end

  def initialize(name = "t")
    @name = name
  end

  def name
    @name
  end
end
"##;

const NAMED: &str = r##"module Named
  class Widget
    def self.kind
      "widget"
    end
  end
end
"##;

/// A class alias of an app class.
const REMOTE_THING: &str = "# frozen_string_literal: true\n\nRemoteThing = Thing\n";

/// A class alias of a namespaced app class.
const OLD_WIDGET: &str = "OldWidget = Named::Widget\n";

/// A class alias in its lexical owner.
const LEGACY: &str = "module Legacy\n  Gadget = Thing\nend\n";

/// A class alias of a core class.
const LOOSE_STRING: &str = "LooseString = String\n";

/// Block-less factory classes.
const PAIR: &str = "Pair = Struct.new(:left, :right)\n";
const OPTIONS: &str = "Options = Struct.new(:size, :color, keyword_init: true)\n";
const SPOT: &str = "Spot = Data.define(:x, :y)\n";

/// A factory class with a body block, alone in its file (Core's
/// `ViewLineItem`): before, a file with no class or module node was
/// not a library file at all under app/models, so this was dropped too.
const BADGE: &str = r##"Badge = Struct.new(:label, keyword_init: true) do
  def shout
    label.upcase
  end
end
"##;

/// Another file, reading every one of them.
const READER: &str = r##"class Reader
  def self.read
    pair = Pair.new(1, 2)
    pair.right = 5
    options = Options.new(size: 3, color: "red")
    spot = Spot.new(x: 1, y: 2)
    [
      RemoteThing.kind,
      RemoteThing.new("r").name,
      OldWidget.kind,
      LooseString.new("s") + "!",
      pair.left + pair.right,
      pair.to_a,
      options.size,
      options.color,
      spot.x + spot.y,
      spot.with(y: 7).y,
      Badge.new(label: "hi").shout,
      RemoteThing <= Thing,
      Thing.new.is_a?(RemoteThing),
      RemoteThing.name,
      LooseString === "s",
      Legacy::Gadget.kind,
      Legacy::Gadget <= Thing,
    ]
  end
end
"##;

const FILES: [(&str, &str); 11] = [
    ("app/models/thing.rb", THING),
    ("app/models/named.rb", NAMED),
    ("app/models/remote_thing.rb", REMOTE_THING),
    ("app/models/old_widget.rb", OLD_WIDGET),
    ("app/models/loose_string.rb", LOOSE_STRING),
    ("app/models/pair.rb", PAIR),
    ("app/models/options.rb", OPTIONS),
    ("app/models/spot.rb", SPOT),
    ("app/models/badge.rb", BADGE),
    ("app/models/legacy.rb", LEGACY),
    ("app/models/reader.rb", READER),
];

const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message]
  end
  puts "#{label}: #{value.inspect}"
end
seen("reader") { Reader.read }
seen("alias") { [RemoteThing.equal?(Thing), RemoteThing.name, OldWidget.name, LooseString.equal?(String)] }
seen("factories") { [Pair.superclass, Options.superclass, Spot.superclass, Pair.members, Options.keyword_init?] }
seen("pair") { Pair.new(1, 2) == Pair.new(1, 2) }
seen("spot") { Spot.new(x: 1, y: 2).inspect }
"##;

const EXPECTED: [&str; 5] = [
    "reader: [\"thing\", \"r\", \"widget\", \"s!\", 6, [1, 5], 3, \"red\", 3, 7, \"HI\", true, true, \"Thing\", true, \"thing\", true]",
    "alias: [true, \"Thing\", \"Named::Widget\", true]",
    "factories: [Struct, Struct, Data, [:left, :right], true]",
    "pair: true",
    "spot: \"#<data Spot x=1, y=2>\"",
];

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n");
    for (path, source) in FILES {
        app = app.write(path, source);
    }
    app
}

/// The same files on plain Ruby, required in order.
fn native() -> String {
    // Each call gets its own directory, so concurrent tests never share one.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_constant_only_native_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(dir.join("app/models")).unwrap();
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
fn constant_only_files_run_like_the_native_run() {
    let native = native();
    assert_eq!(native.lines().collect::<Vec<_>>(), EXPECTED, "native:\n{native}");
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    assert_eq!(emitted.stdout.lines().collect::<Vec<_>>(), EXPECTED, "emitted:\n{}", emitted.stdout);
}

/// Each file autoloads under its constant, as Zeitwerk maps it by name.
#[test]
fn each_constant_only_file_autoloads_under_its_constant() {
    let (dir, _) = app().emit(BuildTarget::Ruby);
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    for entry in [
        r#"[:RemoteThing, "models/remote_thing", false]"#,
        r#"[:OldWidget, "models/old_widget", false]"#,
        r#"[:LooseString, "models/loose_string", false]"#,
        r#"[:Pair, "models/pair", false]"#,
        r#"[:Options, "models/options", false]"#,
        r#"[:Spot, "models/spot", false]"#,
        r#"[:Badge, "models/badge", false]"#,
        r#"[:Legacy, "models/legacy", true]"#,
    ] {
        assert!(models.contains(entry), "{entry} missing:\n{models}");
    }
    let boot = &models[models.find("BOOT_FILES = [").expect("BOOT_FILES")..];
    let boot = &boot[..boot.find("].freeze").unwrap()];
    assert!(boot.trim() == "BOOT_FILES = [", "{boot}");
}

/// Only the Ruby family writes a file of bare top-level constants;
/// another target would have to invent a class for it.
#[test]
fn other_targets_refuse_a_file_of_constants() {
    let tree: roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> = [
        ("app/models/thing.rb", THING),
        ("app/models/remote_thing.rb", REMOTE_THING),
    ]
    .into_iter()
    .map(|(path, source)| (std::path::PathBuf::from(path), source.as_bytes().to_vec()))
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    let (result, _) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(&app, std::path::Path::new("not-a-fixture"), BuildTarget::Typescript)
    });
    let error = result.expect_err("typescript emitted a file of constants");
    assert!(error.contains("RemoteThing is a file of top-level constants"), "{error}");
}

/// An alias of a class the app does not have is reported, at the alias
/// and where it is read, not silently written into a file that raises
/// NameError when it loads.
#[test]
fn an_alias_of_a_missing_class_is_reported() {
    let (_, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/ghost.rb", "Ghost = Missing::Thing\n")
        .write("app/models/haunt.rb", "class Haunt\n  def self.ghost\n    Ghost.name\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(errors.iter().any(|error| error.contains("constant not supported (all targets): Missing::Thing")), "{errors:?}");
    assert!(errors.iter().any(|error| error.ends_with("constant not supported (all targets): Ghost")), "{errors:?}");
}

/// A file of sorbet type aliases alone that no check reads leaves
/// nothing to write: sorbet-runtime never evaluates such an alias, so
/// the tree neither writes nor loads the file.
#[test]
fn a_file_of_unread_type_aliases_is_not_written() {
    let (dir, errors) = app()
        .write("app/models/loose_kind.rb", "LooseKind = T.type_alias { T.any(String, Symbol) }\n")
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!dir.join("app/models/loose_kind.rb").exists());
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    assert!(!models.contains("loose_kind"), "{models}");
}
