//! A class whose body reads its own nested subclass (kept in a file of
//! its own) loads as Zeitwerk loads it: the outer class is defined, its
//! body reaches the subclass, the subclass's file loads, and neither file
//! requires the other while it is loading.
//!
//! Core's components/markets/app/models/markets/region.rb:
//!
//! ```ruby
//! class Region < GlobalDB::ShopRecord
//!   PRIORITIES = {
//!     Markets::Region::Country.name => 1,
//!     Markets::Region::Subdivision.name => 2,
//!   }
//! ```
//!
//! with `class Country < Markets::Region` in region/country.rb. The
//! emitted region.rb required region/country above its class, and
//! country.rb required region.rb back for its superclass: a circular
//! require (Ruby's no-op, warning), which Core's StrictWarning raised at
//! test boot.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const BASE: &str = "class BaseRecord\n  def self.kind\n    \"record\"\n  end\nend\n";
const REGION: &str = "class Region < BaseRecord\n  PRIORITIES = {\n    Region::Country.name => 1,\n    Region::Subdivision.name => 2,\n  }.freeze\n\n  def self.priority\n    PRIORITIES.fetch(name, 0)\n  end\nend\n";
const COUNTRY: &str = "class Region < BaseRecord\n  class Country < Region\n    def self.code\n      \"C\"\n    end\n  end\nend\n";
const SUBDIVISION: &str = "class Region < BaseRecord\n  class Subdivision < Region\n    def self.code\n      \"S\"\n    end\n  end\nend\n";

/// Raises on a circular-require warning, as Core's StrictWarning does for
/// one located in the app.
const HOOK: &str = "module CircularHook\n  def warn(msg, category: nil, **)\n    raise \"strict warning: #{msg}\" if msg.include?(\"circular require\")\n    super\n  end\nend\nWarning.singleton_class.prepend(CircularHook)\n$VERBOSE = true\n";

const SCRIPT: &str = "puts Region::PRIORITIES.keys.join(\" \")\nputs [Region::Country.code, Region::Subdivision.code].join\nputs Region::Country.priority\nputs Region::Subdivision.priority\nputs Region::Country.superclass\nputs Region::Country.kind\n";

const EXPECTED: [&str; 6] = ["Region::Country Region::Subdivision", "CS", "1", "2", "Region", "record"];

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_class_body_reading_its_nested_subclass_loads_in_zeitwerk_order() {
    let root = std::env::temp_dir().join(format!("roundhouse-nested-subclass-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("region")).unwrap();
    for (path, text) in [("base_record.rb", BASE), ("region.rb", REGION), ("region/country.rb", COUNTRY), ("region/subdivision.rb", SUBDIVISION)] {
        std::fs::write(root.join(path), text).unwrap();
    }
    // The hook goes on after the tree's boot, which loads neither file:
    // each loads at its first reference below.
    // CRuby with Zeitwerk managing the directory, as Rails' autoloader.
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{HOOK}require \"zeitwerk\"\nloader = Zeitwerk::Loader.new\nloader.push_dir({:?})\nloader.setup\n{SCRIPT}", root.display().to_string()))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(lines(&native.stdout), EXPECTED, "native control");

    let (tree, errors) = emit_and_run::empty_app()
        .write("app/models/base_record.rb", BASE)
        .write("app/models/region.rb", REGION)
        .write("app/models/region/country.rb", COUNTRY)
        .write("app/models/region/subdivision.rb", SUBDIVISION)
        .emit(BuildTarget::Ruby);
    let region = std::fs::read_to_string(tree.join("app/models/region.rb")).unwrap_or_default();
    assert!(!region.contains("require_relative \"region/country\""), "region.rb requires its subclass above its class:\n{region}\nerrors: {errors:#?}");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{HOOK}{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    assert_eq!(lines(&run.stdout), EXPECTED, "{stderr}\nerrors: {errors:#?}");

    // The subclass's file loading first (another file's require of it)
    // loads the outer class's file first, still without a warning.
    let first = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{HOOK}require File.join(Dir.pwd, \"app/models/region/country\")\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(first.status.success(), "emitted tree (subclass first) failed in {}: {stderr}", tree.display());
    assert_eq!(lines(&first.stdout), EXPECTED, "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
