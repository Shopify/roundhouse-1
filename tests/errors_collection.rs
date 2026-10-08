//! Errors operations without a shared implementation remain named refusals.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;

fn dispatch_failures(action: &str) -> Vec<String> {
    let model = format!(
        "class Checkout\n  include ActiveModel::Validations\n\n  def check(other)\n{action}\n  end\nend\n"
    );
    let tree: HashMap<PathBuf, Vec<u8>> = [("app/models/checkout.rb", model)]
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.into_bytes()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let residue = roundhouse::session::analyze_and_lower(&mut app);
    residue
        .iter()
        .chain(roundhouse::analyze::diagnose(&app).iter())
        .map(roundhouse::diagnostic::Diagnostic::to_string)
        .filter(|d| d.contains("send_dispatch_failed") || d.contains("unsupported"))
        .collect()
}

#[test]
fn unimplemented_errors_operations_are_refused() {
    let diags = dispatch_failures(
        "    errors.copy!(other.errors)\n    errors.group_by_attribute\n    errors.added?(:base, :blank)\n    errors.import(other.errors.first)\n    errors.merge!(other.errors)\n    errors.delete(:base)\n    errors.where(:base).any?\n    errors.details[:base]\n    errors.map(&:type).include?(:x)\n    errors.errors[0].options[:code]\n    errors.attribute_names\n    errors.messages",
    );
    for name in ["copy!", "group_by_attribute"] {
        assert!(diags.iter().any(|d| d.contains(name)), "missing {name}: {diags:?}");
    }
    // `attribute_names` has a shared implementation since 51fc4a07c
    // (runtime/spinel/active_model_errors.rb), which
    // tests/errors_association_ports.rs runs against Core's pinned
    // activemodel; the Spinel tree still refuses it there.
    assert!(!diags.iter().any(|d| d.contains("attribute_names")), "{diags:?}");
    // So have `merge!`, `details`, Enumerable over the Errors, the
    // queries by attribute, `delete` and `import`
    // (tests/errors_surface_port.rs, against native activerecord on
    // SQLite and on the task PostgreSQL).
    for name in ["merge!", "details", "added?", "import", "delete", "where", " map ", "options"] {
        assert!(!diags.iter().any(|d| d.contains(name)), "{name}: {diags:?}");
    }
}
