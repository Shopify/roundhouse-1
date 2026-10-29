//! Active Support core extensions a native gem requires as it loads, on the
//! PostgreSQL CRuby tree: `core_ext/array/access`, `array/wrap`,
//! `enumerable`, `string/filters`, `module/delegation` (with
//! `active_support/delegation`) and `hash/indifferent_access` (with
//! `active_support/hash_with_indifferent_access`). Each answers from a
//! runtime port of Rails' own file (runtime/spinel/active_support_*.rb,
//! verbatim) or, for the indifferent hash, the tree's existing port. Before,
//! the boundary refused each one by name
//! (error[framework_feature_not_ported]), so every gem that required one
//! did not load: a large Rails monolith's gem probe found these six behind
//! 22 of its 51 Rails-dependent gems.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

fn tree() -> emit_and_run::Emitted {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_active_support_gem_features\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    dir
}

/// The pieces of boot.rb a gem's require runs under: the hooks, the
/// features path and refusal runtime/gem_facades.rb sets up, and the ports
/// `require "rails"` has loaded.
const PRELUDE: &str = "require_relative 'runtime/active_support_lazy_load_hooks'\n\
    $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
    require_relative 'runtime/framework_feature_refusal'\n\
    require_relative 'runtime/module_delegate'\n\
    require_relative 'runtime/active_support_inflector'\n\
    require_relative 'runtime/active_support_core_ext'\n";

fn run(dir: &std::path::Path, script: &str) -> String {
    let out = emit_and_run::ruby()
        .current_dir(dir)
        .args(["-e", &format!("{PRELUDE}{script}")])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

#[test]
fn a_gem_s_core_extension_requires_answer_from_the_ports() {
    let dir = tree();
    for feature in [
        "active_support/core_ext/array/access",
        "active_support/core_ext/array/wrap",
        "active_support/core_ext/enumerable",
        "active_support/core_ext/string/filters",
        "active_support/core_ext/module/delegation",
        "active_support/delegation",
        "active_support/core_ext/hash/indifferent_access",
        "active_support/hash_with_indifferent_access",
    ] {
        assert!(dir.join(format!("runtime/features/{feature}.rb")).exists(), "runtime/features/{feature}.rb");
    }
    let stdout = run(
        &dir,
        "require 'active_support/core_ext/array/access'\n\
         require 'active_support/core_ext/array/wrap'\n\
         require 'active_support/core_ext/enumerable'\n\
         require 'active_support/core_ext/string/filters'\n\
         require 'active_support/core_ext/module/delegation'\n\
         require 'active_support/core_ext/hash/indifferent_access'\n\
         p [[1, 2, 3, 4].second, [1, 2, 3, 4].from(2), [1, 2, 3].excluding(2), [1, 2].including(3)]\n\
         p [Array.wrap(nil), Array.wrap([1]), Array.wrap(1)]\n\
         p [%w[a bb].index_by(&:size), [{ a: 1 }, { a: 2 }].pluck(:a), [1, 2, 2].many?, [7].sole, [1, nil, ''].compact_blank]\n\
         begin\n  [1, 2].sole\nrescue => e\n  p e.class.equal?(ActiveSupport::EnumerableCoreExt::SoleItemExpectedError)\nend\n\
         p [\" a  b \\n c \".squish, 'Once upon a time in a world far far away'.truncate(27), 'hello world'.remove('o'), 'a b c d'.truncate_words(2)]\n\
         class Wrapper\n  def initialize(target) = @target = target\n  delegate_missing_to :@target\nend\n\
         class Holder\n  attr_reader :items\n  def initialize = @items = [1, 2, 3]\n  delegate :size, to: :items, prefix: true\n  delegate :name, to: :missing, allow_nil: true\n  def missing = nil\nend\n\
         p [Wrapper.new([1, 2]).size, Wrapper.new([1, 2]).respond_to?(:size), Holder.new.items_size, Holder.new.name]\n\
         begin\n  Wrapper.new(nil).size\nrescue => e\n  p e.class\nend\n\
         h = { a: 1 }.with_indifferent_access\n\
         p [h['a'], h[:a], h.class.name]\n\
         p $LOADED_FEATURES.grep(%r{/activesupport[-/]}).empty?\n",
    );
    assert_eq!(
        stdout,
        "[2, [3, 4], [1, 3], [1, 2, 3]]\n\
         [[], [1], [1]]\n\
         [{1 => \"a\", 2 => \"bb\"}, [1, 2], true, 7, [1]]\n\
         true\n\
         [\"a b c\", \"Once upon a time in a wo...\", \"hell wrld\", \"a b...\"]\n\
         [2, true, 3, nil]\n\
         ActiveSupport::DelegationError\n\
         [1, 1, \"ActiveSupport::HashWithIndifferentAccess\"]\n\
         true\n"
    );
}

/// A core extension the tree still does not port stays refused by name.
#[test]
fn an_unported_core_extension_is_still_refused_by_name() {
    let dir = tree();
    let stdout = run(
        &dir,
        "begin\n  require 'active_support/core_ext/array/grouping'\nrescue LoadError => e\n  puts e.message[/framework_feature_not_ported/]\nend\n",
    );
    assert_eq!(stdout, "framework_feature_not_ported\n");
}
