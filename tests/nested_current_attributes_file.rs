//! A `CurrentAttributes` class nested in a class that keeps a file of its
//! own is written in that file, not in the outermost class's.
//!
//! Core's `components/markets/app/models/markets/shop_scope/cached.rb`
//! reopens `Markets::ShopScope` around `class Cached`, whose
//! `class Current < ActiveSupport::CurrentAttributes` the ingest lowers to
//! generated methods with no source spans. `file_owner` placed a class
//! with no files of its own in the OUTERMOST enclosing class's file, so
//! `shop_scope.rb` opened `class Cached; class Current`: loading
//! shop_scope.rb first (an initializer's GraphQL types reach it before
//! anything reads `ShopScope::Cached`) ran `class Cached`, which
//! autoloaded cached.rb, whose `require_relative "../shop_scope"` was a
//! circular require Core's StrictWarning raises: test boot died. Natively
//! shop_scope.rb never names `Cached`; Zeitwerk loads cached.rb when it is
//! first read.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const SCOPE: &str = "class Scope\n  attr_reader :id\n\n  def initialize(id)\n    @id = id\n  end\nend\n";
const SHOP_SCOPE: &str = "class ShopScope < Scope\n  def label\n    \"shop #{id}\"\n  end\nend\n";
const CACHED: &str = "class ShopScope\n  class Cached\n    class Current < ActiveSupport::CurrentAttributes\n      attribute :shop_scope\n    end\n    private_constant :Current\n\n    def self.for(id)\n      current = Current.shop_scope\n      return current if current&.id == id\n\n      Current.shop_scope = ShopScope.new(id)\n    end\n  end\nend\n";

/// Raises on a circular-require warning, as Core's StrictWarning does for
/// one located in the app.
const HOOK: &str = "module CircularHook\n  def warn(msg, category: nil, **)\n    raise \"strict warning: #{msg}\" if msg.include?(\"circular require\")\n    super\n  end\nend\nWarning.singleton_class.prepend(CircularHook)\n$VERBOSE = true\n";

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_current_nested_in_a_class_written_apart_stays_in_that_classs_file() {
    let (tree, errors) = emit_and_run::empty_app()
        .write("app/models/scope.rb", SCOPE)
        .write("app/models/shop_scope.rb", SHOP_SCOPE)
        .write("app/models/shop_scope/cached.rb", CACHED)
        .emit(BuildTarget::Ruby);
    let shop_scope = std::fs::read_to_string(tree.join("app/models/shop_scope.rb"))
        .unwrap_or_else(|e| panic!("shop_scope.rb: {e}\nerrors: {errors:#?}"));
    assert!(!shop_scope.contains("class Cached"), "the outermost class's file opens the nested class:\n{shop_scope}");
    let cached = std::fs::read_to_string(tree.join("app/models/shop_scope/cached.rb"))
        .unwrap_or_else(|e| panic!("shop_scope/cached.rb: {e}\nerrors: {errors:#?}"));
    assert!(cached.contains("class Current"), "the Current class is not in its own class's file:\n{cached}");

    // ShopScope loads first, as Core's initializer reaches it, then the
    // cache; each id answers its own scope, and the second read of an id
    // is the cached instance.
    let script = format!(
        "{HOOK}require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\nputs ShopScope.new(1).label\nfirst = ShopScope::Cached.for(2)\nputs first.label\nputs ShopScope::Cached.for(2).equal?(first)\nputs ShopScope::Cached.for(3).label\n"
    );
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(script)
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    assert_eq!(lines(&run.stdout), ["shop 1", "shop 2", "true", "shop 3"], "{stderr}\nerrors: {errors:#?}");
}
