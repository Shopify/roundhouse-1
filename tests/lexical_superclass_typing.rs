//! The analyzer types a class with the superclass Ruby binds, where a
//! namespace and the top level both define the written name. Ruby reads
//! the superclass in the lexical nesting at the `class` keyword,
//! innermost first, then the top level: inside `module Errs`, `Fallback`
//! is `Errs::Fallback`; in a top-level compact `class Errs::Unavailable
//! < Fallback` it is the top-level `Fallback`. Rubydex binds a source
//! superclass at ingest (`resolve_library_parents`, which also covers
//! STI subclasses); `analyze::superclass_binding` binds one only a gem's
//! RBI defines.
//!
//! CRuby is the oracle: each shape loads natively and answers which of
//! the two shadowing classes defines methods for it; the analyzer must
//! resolve the same method and leave the other unresolved.

use std::process::Command;

fn check(files: &[(&str, &str)]) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("rh_lexical_superclass_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ]
    .iter()
    .chain(files)
    {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["check", "--unknown-sends"])
        .arg(&root)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// Native: `[method_defined?(:top_only), method_defined?(:inner_only)]`
/// for each class, after loading `files` in order.
fn native(files: &[(&str, &str)], classes: &[&str]) -> String {
    let mut script = String::new();
    for (_, text) in files {
        script.push_str(text);
        script.push('\n');
    }
    for class in classes {
        script.push_str(&format!("p [{class}.method_defined?(:top_only), {class}.method_defined?(:inner_only)]\n"));
    }
    let output = Command::new("ruby").arg("-W0").arg("-e").arg(&script).output().expect("ruby");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A reader that sends both methods to `class`; the one its ancestry
/// lacks is listed as unresolved.
fn reader(name: &str, class: &str) -> String {
    format!(
        "class {name}\n  #: ({class}) -> Integer\n  def self.top(n) = n.top_only\n\n  #: ({class}) -> Integer\n  def self.inner(n) = n.inner_only\nend\n"
    )
}

fn unresolved(text: &str, file: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.contains(file) && line.contains("resolves to no method on its known receiver"))
        .filter_map(|line| line.split('`').nth(1).map(str::to_string))
        .collect()
}

const TOP_FALLBACK: (&str, &str) = ("app/lib/fallback.rb", "class Fallback < StandardError\n  def top_only = 1\nend\n");

#[test]
fn library_superclasses_bind_as_ruby_reads_them() {
    let errs = (
        "app/lib/errs.rb",
        "module Errs\n  class Fallback < StandardError\n    def inner_only = 2\n  end\n\n  class Nested < Fallback\n  end\n\n  module Platform\n    class Base\n      def inner_only = 2\n    end\n  end\n\n  class Deep < Platform::Base\n  end\nend\n",
    );
    let compact = ("app/lib/errs/unavailable.rb", "class Errs::Unavailable < Fallback\nend\n");
    let platform = ("app/lib/platform/base.rb", "module Platform\n  class Base\n    def top_only = 1\n  end\nend\n");
    let files = [TOP_FALLBACK, errs, compact, platform];
    let classes = ["Errs::Nested", "Errs::Unavailable", "Errs::Deep"];
    assert_eq!(native(&files, &classes), "[false, true]\n[true, false]\n[false, true]\n");

    let readers: Vec<(String, String)> = [("NestedReader", "Errs::Nested"), ("CompactReader", "Errs::Unavailable"), ("DeepReader", "Errs::Deep")]
        .iter()
        .map(|(name, class)| (format!("app/lib/{}.rb", name.to_lowercase()), reader(name, class)))
        .collect();
    let mut all: Vec<(&str, &str)> = files.to_vec();
    all.extend(readers.iter().map(|(p, t)| (p.as_str(), t.as_str())));
    let text = check(&all);
    assert_eq!(unresolved(&text, "nestedreader.rb"), ["top_only"], "{text}");
    assert_eq!(unresolved(&text, "compactreader.rb"), ["inner_only"], "{text}");
    assert_eq!(unresolved(&text, "deepreader.rb"), ["top_only"], "{text}");
}

#[test]
fn a_namespaced_sti_subclass_binds_the_namespace_model() {
    let schema = (
        "db/schema.rb",
        "ActiveRecord::Schema.define do\n  create_table :payment_methods do |t|\n    t.string :type\n  end\n  create_table :pay_payment_methods do |t|\n    t.string :type\n  end\nend\n",
    );
    let top = ("app/models/payment_method.rb", "class PaymentMethod < ApplicationRecord\n  def top_only = 1\nend\n");
    let pay = ("app/models/pay.rb", "module Pay\n  def self.table_name_prefix = \"pay_\"\nend\n");
    let inner = ("app/models/pay/payment_method.rb", "module Pay\n  class PaymentMethod < ApplicationRecord\n    def inner_only = 2\n  end\nend\n");
    let card = ("app/models/pay/card.rb", "module Pay\n  class Card < PaymentMethod\n  end\nend\n");
    let native_files = [
        ("", "require \"active_record\"\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        top,
        pay,
        inner,
        card,
    ];
    assert_eq!(native(&native_files, &["Pay::Card"]), "[false, true]\n");

    let card_reader = reader("CardReader", "Pay::Card");
    let text = check(&[schema, top, pay, inner, card, ("app/lib/card_reader.rb", &card_reader)]);
    assert_eq!(unresolved(&text, "card_reader.rb"), ["top_only"], "{text}");
}

/// The methods each `class` reaches, by the `#: (class) -> untyped`
/// reader's sends on what `top_only` and `inner_only` return: a found
/// method answers an `Integer`, which has no `zzz_*`.
fn found(text: &str, file: &str, class: &str) -> Vec<&'static str> {
    let unresolved_on_integer = |probe: &str| {
        text.lines().any(|line| {
            line.contains(file)
                && line.contains(&format!("send `{probe}` resolves to no method on its known receiver"))
        })
    };
    let tag = class.replace("::", "_").to_lowercase();
    [("top_only", format!("zzz_top_{tag}")), ("inner_only", format!("zzz_inner_{tag}"))]
        .into_iter()
        .filter(|(_, probe)| unresolved_on_integer(probe))
        .map(|(name, _)| name)
        .collect()
}

fn probes(classes: &[&str]) -> String {
    let mut text = String::from("class Probes\n");
    for class in classes {
        let tag = class.replace("::", "_").to_lowercase();
        text.push_str(&format!(
            "  #: ({class}) -> untyped\n  def self.top_{tag}(n) = n.top_only.zzz_top_{tag}\n\n  #: ({class}) -> untyped\n  def self.inner_{tag}(n) = n.inner_only.zzz_inner_{tag}\n\n"
        ));
    }
    text.push_str("end\n");
    text
}

/// A superclass only a gem's RBI defines gets no rubydex rewrite (rubydex
/// indexes the app's sources), so the analyzer binds it: Core's
/// `Impertinent::LiquidColor < Script::Color` inside `module
/// Impertinent`, and rack-oauth2's `Token::Request` read from a class
/// nested under `Rack::OAuth2::Server::Token`.
#[test]
fn a_gem_superclass_binds_through_the_nesting() {
    let gem = "module Vend\n  module Script\n    class Color\n      def inner_only = 2\n    end\n  end\n\n  module Srv\n    module Abstract\n      class Handler\n        def inner_only = 2\n      end\n    end\n\n    class Token\n      class Request\n        def inner_only = 2\n      end\n    end\n  end\nend\n";
    let rbi = "# typed: true\n\nmodule Vend; end\nmodule Vend::Script; end\n\nclass Vend::Script::Color\n  sig { returns(Integer) }\n  def inner_only; end\nend\n\nmodule Vend::Srv; end\nmodule Vend::Srv::Abstract; end\n\nclass Vend::Srv::Abstract::Handler\n  sig { returns(Integer) }\n  def inner_only; end\nend\n\nclass Vend::Srv::Token; end\n\nclass Vend::Srv::Token::Request\n  sig { returns(Integer) }\n  def inner_only; end\nend\n";
    let sources = [
        ("app/lib/script.rb", "class Script\n  class Color\n    def top_only = 1\n  end\nend\n"),
        ("app/lib/base.rb", "class Base\n  def top_only = 1\nend\n"),
        ("app/lib/vend/functions.rb", "module Vend\n  module Script\n    module Functions\n    end\n  end\nend\n"),
        (
            "app/lib/vend/liquid_color.rb",
            "module Vend\n  class LiquidColor < Script::Color\n  end\n\n  class RootedColor < ::Script::Color\n  end\n\n  class Base < Base\n  end\nend\n",
        ),
        (
            "app/lib/vend/srv/token/bearer.rb",
            "module Vend\n  module Srv\n    class Token\n      module Ext\n        class Bearer < Abstract::Handler\n          class Request < Token::Request\n          end\n        end\n      end\n    end\n  end\nend\n",
        ),
    ];
    let classes = [
        "Vend::LiquidColor",
        "Vend::RootedColor",
        "Vend::Base",
        "Vend::Srv::Token::Ext::Bearer",
        "Vend::Srv::Token::Ext::Bearer::Request",
    ];
    let mut native_files = vec![("", gem)];
    native_files.extend(sources);
    assert_eq!(
        native(&native_files, &classes),
        "[false, true]\n[true, false]\n[true, false]\n[false, true]\n[false, true]\n"
    );

    let probes = probes(&classes);
    let mut files = vec![
        ("Gemfile", "source \"https://rubygems.org\"\ngem \"vend\"\n"),
        ("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    vend (1.0.0)\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  vend\n\nBUNDLED WITH\n   2.5.0\n"),
        ("sorbet/rbi/gems/vend@1.0.0.rbi", rbi),
        ("app/lib/probes.rb", probes.as_str()),
    ];
    files.extend(sources);
    let text = check(&files);
    let answers: Vec<Vec<&str>> = classes.iter().map(|class| found(&text, "probes.rb", class)).collect();
    assert_eq!(
        answers,
        [vec!["inner_only"], vec!["top_only"], vec!["top_only"], vec!["inner_only"], vec!["inner_only"]],
        "{text}"
    );
}

/// A superclass that is a constant of the enclosing class (Core's
/// `OrderEditSummary::DeliveryChangeParent = Struct.new(...)`) is no
/// source class for rubydex to name, and a top-level class of the same
/// name must not take its place.
#[test]
fn a_struct_constant_superclass_binds_in_its_class() {
    let top = ("app/lib/change_parent.rb", "class ChangeParent\n  def top_only = 1\nend\n");
    let summary = (
        "app/lib/summary.rb",
        "class Summary\n  ChangeParent = Struct.new(:inner_only)\n\n  class Change < ChangeParent\n  end\nend\n",
    );
    assert_eq!(native(&[top, summary], &["Summary::Change"]), "[false, true]\n");

    let reader = reader("ChangeReader", "Summary::Change");
    let text = check(&[top, summary, ("app/lib/change_reader.rb", &reader)]);
    assert_eq!(unresolved(&text, "change_reader.rb"), ["top_only"], "{text}");
}
