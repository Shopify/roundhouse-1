//! Class-body statements that call a macro the class gets from a module
//! its base `extend`s, a `delegate` forwarder aliased below its
//! declaration, and a mixin statement in a body that already includes.
//!
//! Core's `Pagination::ProductCollectionInSqlQuery` loops over
//! `CollectionSorter.all.pluck(...)` calling `sort_param`, which
//! `Pagination::Query` gets from `extend CompositionHelper`; dropped, the
//! GraphQL sort-key enum lacked every collection sorter. frozen_record's
//! `Scope` has `alias_method :find_each, :each` below a `delegate` another
//! class's `def each(*)` declines. Each emitted class must answer what CRuby
//! answers, and nothing may be dropped silently.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const QUERY: &str = r##"module Pagination
  class Query
    module CompositionHelper
      def sort_param(key, type = nil)
        sort_param_definitions << [key.to_s, type]
      end

      def sort_param_definitions
        @sort_param_definitions ||= []
      end
    end

    extend CompositionHelper
  end
end
"##;

const SORTER: &str = r##"module Sorter
  def self.handles
    [["best-selling", "string"], ["title", "string"], ["created", "integer"]]
  end
end
"##;

const SUB_QUERY: &str = r##"module Pagination
  class SubQuery < Pagination::Query
    Sorter.handles.each do |handle, data_type|
      sort_param handle, data_type.to_sym
    end
    sort_param "default", :string
  end
end
"##;

const SCOPE: &str = r##"class Scope
  delegate :first, :each, :count, to: :to_a
  alias_method :find_each, :each

  def initialize(items)
    @items = items
  end

  def to_a
    @items
  end
end
"##;

/// Declines `each` for the delegate above (a `(*)` rest nameless).
const DECLINER: &str = r##"class Decliner
  def each(*)
    yield 1
  end
end
"##;

const MARKER: &str = r##"module Marker
  def marker
    "marked"
  end
end
"##;

const TAGGED: &str = r##"class Tagged
  include Comparable
  unsafe_self = self #: as untyped
  unsafe_self.include(Marker)

  attr_reader :n

  def initialize(n)
    @n = n
  end

  def <=>(other)
    n <=> other.n
  end
end
"##;

const SCRIPT: &str = r##"
puts Pagination::SubQuery.sort_param_definitions.inspect
s = Scope.new([3, 4])
found = []
s.find_each { |i| found << i }
puts found.inspect
puts Scope.instance_method(:find_each).original_name.inspect
t = Tagged.new(1)
puts [t.marker, Tagged.include?(Comparable), Tagged.include?(Marker), Tagged.ancestors.index(Marker) < Tagged.ancestors.index(Comparable)].inspect
"##;

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn class_body_macros_from_extended_modules_and_aliased_forwarders_run_as_in_cruby() {
    let sources = [QUERY, SORTER, SUB_QUERY, SCOPE, DECLINER, MARKER, TAGGED].join("\n");
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"active_support/all\"\n{sources}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(expected.len(), 4, "native control: {expected:?}");
    assert!(expected[0].contains("best-selling") && expected[0].contains("default"), "{expected:?}");

    let (tree, errors) = emit_and_run::empty_app()
        .write("lib/pagination/query.rb", QUERY)
        .write("lib/sorter.rb", SORTER)
        .write("lib/pagination/sub_query.rb", SUB_QUERY)
        .write("lib/scope.rb", SCOPE)
        .write("lib/decliner.rb", DECLINER)
        .write("lib/marker.rb", MARKER)
        .write("lib/tagged.rb", TAGGED)
        .emit(BuildTarget::Ruby);
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby line for line\nerrors: {errors:#?}");
    let dropped: Vec<_> = errors
        .iter()
        .filter(|e| format!("{e:?}").contains("is not modelled and is dropped"))
        .collect();
    assert!(dropped.is_empty(), "{dropped:#?}");
}

const KIND: &str = r##"module Banking
  class TermsOfServiceKind < T::Enum
    enums do
      Cardholder = new("cardholder")
      Banking = new("banking")
      Credit = new("credit")
    end
  end
end
"##;

/// A member named for the enum's own namespace (`Banking = new` in
/// `Banking::TermsOfServiceKind`) shadows the namespace inside the class: the
/// members after it, and `values`, must name the class from the top level.
#[test]
fn a_t_enum_member_named_for_its_namespace_does_not_shadow_the_class() {
    let script = "puts Banking::TermsOfServiceKind.values.map(&:serialize).inspect\nputs Banking::TermsOfServiceKind::Banking.inspect\nputs Banking::TermsOfServiceKind.from_serialized(\"credit\").inspect";
    let (tree, errors) = emit_and_run::empty_app().write("lib/banking/terms_of_service_kind.rb", KIND).emit(BuildTarget::Ruby);
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{script}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    assert_eq!(
        lines(&run.stdout),
        [
            "[\"cardholder\", \"banking\", \"credit\"]",
            "#<Banking::TermsOfServiceKind::Banking>",
            "#<Banking::TermsOfServiceKind::Credit>"
        ],
        "errors: {errors:#?}"
    );
}
