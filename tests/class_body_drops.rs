//! A class-body statement roundhouse does not lower is named where it is
//! dropped, whatever its receiver.
//!
//! A model's unclaimed `Unknown` items used to be reported only when the
//! call was receiverless or on `self` and was not a writer: a call on
//! another object (`relation_delegate_class(ActiveRecord::Relation)
//! .include(Guard)`), a compound write (`self.counter_thing += 1`) and a
//! write through another class (`Registry.count ||= 5`) vanished from the
//! emitted model without a word. A plain class's `Registry.count ||= 0`
//! was reported as "`call`". Each now carries the statement's own name.
//!
//! `relation_delegate_class` is reported, not modelled: the runtime builds
//! a plain `ActiveRecord::Relation` for every model (there is no per-model
//! relation or `CollectionProxy` class to include `Guard` into).

use std::path::PathBuf;

use roundhouse::det::HashMap;
use roundhouse::project::BuildTarget;
use roundhouse::ingest::ingest_app_from_tree;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

const WIDGET: &str = r#"class Widget < ApplicationRecord
  module Guard
    def guarded?
      true
    end
  end

  def self.counter_thing
    @counter_thing || 0
  end

  def self.counter_thing=(value)
    @counter_thing = value
  end

  relation_delegate_class(ActiveRecord::Relation).include(Guard)
  self.counter_thing += 1
  Registry.count ||= 5
end
"#;

const REGISTRY: &str = "class Registry\n  class_attribute :count\nend\n";

const LEDGER: &str = "class Ledger\n  Registry.count ||= 0\nend\n";

#[test]
fn dropped_class_body_statements_are_named_at_their_site() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/widget.rb", WIDGET),
        ("app/models/registry.rb", REGISTRY),
        ("app/models/ledger.rb", LEDGER),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let ((app, files), diags) = roundhouse::emit::diagnostics::scope(|| {
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        roundhouse::session::analyze_and_lower(&mut app);
        let files = roundhouse::project::target_files(&app, std::path::Path::new("not-a-fixture"), BuildTarget::Ruby);
        (app, files)
    });
    files.expect("ruby files");
    let site = |d: &roundhouse::diagnostic::Diagnostic| {
        let path = (d.span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)).map(|f| f.path.as_str());
        format!("{}: {d:?}", path.unwrap_or("<synthetic>"))
    };
    let messages: Vec<String> = diags.iter().map(site).collect();
    let expect = |file: &str, needles: &[&str]| {
        assert!(
            messages.iter().any(|m| m.contains(file) && needles.iter().all(|n| m.contains(n))),
            "no diagnostic in {file} naming {needles:?}: {messages:#?}"
        );
    };
    expect("widget.rb", &["relation_delegate_class", "`Widget`", "no per-model relation class"]);
    expect("widget.rb", &["counter_thing=", "`Widget`", "dropped"]);
    expect("widget.rb", &["Registry.count=", "`Widget`", "dropped"]);
    expect("ledger.rb", &["`Registry.count=` in the body of `Ledger`", "dropped"]);
}
