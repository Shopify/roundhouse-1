//! A gem source whose entry registers constants through its own
//! `autoload` wrapper, and whose component files sit outside the
//! `Zeitwerk` path of their constant.
//!
//! polaris 25.0.1's lib/polaris.rb defines `register_component(name,
//! path)` as `autoload(name, "components/polaris/#{path}/#{path}")` and
//! calls it per component (`register_component :Badge, "badge"`), so
//! `Polaris::Badge` lives in lib/components/polaris/badge/badge.rb, a
//! file the constant's name does not map to. Core's helpers `include
//! Polaris::Badge`. The tree must carry the file the autoload names and
//! answer the constant from it, as the gem does natively.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    polarity (1.0.0)\n      activesupport (>= 7.2)\n\nDEPENDENCIES\n  activesupport\n  polarity\n";

const ENTRY: &str = "require \"polarity/component\"\n\nmodule Polarity\n  autoload :Plain, \"polarity/plain\"\n\n  def self.register_component(name, path)\n    autoload(name, \"components/polarity/#{path}/#{path}\")\n  end\n\n  register_component :Badge, \"badge\"\n  register_component :Card, \"card\"\nend\n";

const COMPONENT: &str = "module Polarity\n  class Component\n    def initialize(view)\n      @view = view\n    end\n\n    def tag(text)\n      \"<span class=\\\"#{self.class.name.split(\"::\").last.downcase}\\\">#{text}</span>\"\n    end\n  end\nend\n";

const PLAIN: &str = "module Polarity\n  module Plain\n    def self.name_of = \"plain\"\n  end\nend\n";

const BADGE: &str = "module Polarity\n  module Badge\n    def ui_badge(text)\n      ::Polarity::Badge::Badge.new(self).render(text)\n    end\n\n    class Badge < Polarity::Component\n      def render(text)\n        tag(text)\n      end\n    end\n  end\nend\n";

const CARD: &str = "module Polarity\n  module Card\n    def ui_card(text)\n      ::Polarity::Card::Card.new(self).render(text)\n    end\n\n    class Card < Polarity::Component\n      def render(text)\n        tag(text)\n      end\n    end\n  end\nend\n";

const HELPER: &str = "module BadgeHelper\n  include Polarity::Badge\n\n  def self.plain = Polarity::Plain.name_of\nend\n";

const SCRIPT: &str = "class View\n  include BadgeHelper\nend\nputs View.new.ui_badge(\"new\")\nputs BadgeHelper.plain\nputs Polarity::Card::Card.new(nil).render(\"c\")\n";

fn fake_gem_home() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "rh_gem_source_register_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

fn app(home: &Path) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("test_paths:\n  - test\ngem_home: {}\ngem_sources:\n  polarity: vendor/gems/polarity-1.0.0\n", home.display()))
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"polarity\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/badge_helper.rb", HELPER)
        .write("vendor/gems/polarity-1.0.0/lib/polarity.rb", ENTRY)
        .write("vendor/gems/polarity-1.0.0/lib/polarity/component.rb", COMPONENT)
        .write("vendor/gems/polarity-1.0.0/lib/polarity/plain.rb", PLAIN)
        .write("vendor/gems/polarity-1.0.0/lib/components/polarity/badge/badge.rb", BADGE)
        .write("vendor/gems/polarity-1.0.0/lib/components/polarity/card/card.rb", CARD)
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_registered_component_constant_loads_from_the_file_its_autoload_names() {
    let home = fake_gem_home();
    // CRuby on the gem source itself is the oracle.
    let native_root = home.join("native-polarity");
    for (path, content) in [
        ("lib/polarity.rb", ENTRY),
        ("lib/polarity/component.rb", COMPONENT),
        ("lib/polarity/plain.rb", PLAIN),
        ("lib/components/polarity/badge/badge.rb", BADGE),
        ("lib/components/polarity/card/card.rb", CARD),
        ("app/models/badge_helper.rb", HELPER),
    ] {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    let native = emit_and_run::ruby()
        .arg("-I")
        .arg(native_root.join("lib"))
        .arg("-I")
        .arg(native_root.join("lib/components"))
        .arg("-I")
        .arg(native_root.join("app/models"))
        .args(["-rpolarity", "-rbadge_helper", "-e", SCRIPT])
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(expected, ["<span class=\"badge\">new</span>", "plain", "<span class=\"card\">c</span>"], "native control");

    let (tree, errors) = app(&home).emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    assert!(errors.iter().all(|e| !e.contains("Polarity::")), "{errors:#?}");
    let run = Command::new("ruby")
        .current_dir(&*tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}", tree.display());
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby");
}
