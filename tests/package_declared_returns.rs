//! A Packwerk package's signatures are read like the root app's.
//!
//! Ingest read sorbet `sig`s and RBS-inline `#:` comments only from the
//! root `app/` and `lib/`. A method in `components/<name>/app` kept the
//! return its body inferred even where the author declared one, so in
//! Shopify Core's `StripePaymentMethod::Base`
//!
//! ```ruby
//! #: (Hash[Symbol, untyped] hash) -> Hash[Symbol, untyped]
//! def remove_blank_values(hash)
//!   hash.reject! { |_, value| value.blank? }
//!   hash
//! end
//! ```
//!
//! returned its argument's inferred type: the hashes the `generate_*`
//! callers built, which held other `generate_*` results, which were
//! `remove_blank_values` returns. Those nested one level deeper every
//! other fixpoint round. The declared return bounds the method the way a
//! root-app signature does.
use roundhouse::analyze::Analyzer;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::{ClassId, Symbol, Ty};
use std::path::PathBuf;

const PAYLOAD: &str = r#"module Payments
  class Payload
    def initialize(email, line1)
      @email = email
      @line1 = line1
    end

    def payload
      data = {
        email: @email,
        billing_details: generate_billing_details,
        shipping: generate_shipping,
      }
      remove_blank_values(data)
    end

    def generate_billing_details
      data = {
        email: @email,
        address: generate_address,
      }
      remove_blank_values(data)
    end

    def generate_shipping
      data = {
        name: @email,
        address: generate_address,
        billing: generate_billing_details,
      }
      remove_blank_values(data)
    end

    def generate_address
      data = {
        line1: @line1,
        previous: payload,
      }
      remove_blank_values(data)
    end

    #: (Hash[Symbol, untyped] hash) -> Hash[Symbol, untyped]
    def remove_blank_values(hash)
      hash.reject! { |_, value| value.nil? }
      hash
    end

    extend T::Sig

    sig { returns(String) }
    def label
      @email
    end
  end
end
"#;

fn analyzed(payload_path: &str) -> (Option<Ty>, Option<Ty>, Option<Ty>) {
    let files = [
        ("packwerk.yml", "# package_paths: **/\n"),
        ("package.yml", "enforce_dependencies: true\n"),
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table \"things\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n",
        ),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        ),
        ("components/payments/package.yml", "enforce_dependencies: true\n"),
        (payload_path, PAYLOAD),
    ];
    let tree = files
        .iter()
        .map(|(path, src)| (PathBuf::from(path), src.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("fixture ingests");
    let mut analyzer = Analyzer::new(&app);
    analyzer.analyze(&mut app);
    let method = |name: &str| {
        analyzer
            .class_registry()
            .get(&ClassId(Symbol::from("Payments::Payload")))
            .and_then(|c| c.instance_methods.get(&Symbol::from(name)))
            .cloned()
    };
    (method("remove_blank_values"), method("generate_shipping"), method("label"))
}

fn declared_return(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::Fn { ret, .. } => Some(ret),
        _ => None,
    }
}

fn hash_of_symbol_untyped(ty: &Ty) -> bool {
    matches!(ty, Ty::Hash { key, value } if **key == Ty::Sym && **value == Ty::Untyped)
}

#[test]
fn a_package_methods_inline_signature_bounds_its_return() {
    let (remove, shipping, label) = analyzed("components/payments/app/services/payments/payload.rb");
    let remove = remove.expect("remove_blank_values is registered");
    let ret = declared_return(&remove).unwrap_or_else(|| panic!("declared, not inferred: {remove:?}"));
    assert!(hash_of_symbol_untyped(ret), "{ret:?}");
    let shipping = shipping.expect("generate_shipping is registered");
    assert!(hash_of_symbol_untyped(&shipping), "the caller returns the declared type: {shipping:?}");
    let label = label.expect("label is registered");
    assert_eq!(declared_return(&label), Some(&Ty::Str), "a package's sorbet sig is read too: {label:?}");
}

#[test]
fn a_root_app_methods_inline_signature_still_bounds_its_return() {
    let (remove, shipping, _) = analyzed("app/services/payments/payload.rb");
    let remove = remove.expect("remove_blank_values is registered");
    assert!(declared_return(&remove).is_some_and(hash_of_symbol_untyped), "{remove:?}");
    assert!(shipping.as_ref().is_some_and(hash_of_symbol_untyped), "{shipping:?}");
}

/// `roundhouse check --continue` on a package whose `registry.rb` fails
/// class ingest and declares a signature.
fn check_with_dropped_registry() -> String {
    let root = std::env::temp_dir().join(format!("roundhouse-package-sigs-dropped-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("packwerk.yml", "# package_paths: **/\n"),
        ("package.yml", "enforce_dependencies: true\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        ),
        ("components/payments/package.yml", "enforce_dependencies: true\n"),
        (
            "components/payments/app/lib/payments/registry.rb",
            "module Payments\n  module Registry\n    store = Object.new\n    DEFAULT = store\n\n    #: -> String\n    def self.label\n      \"registry\"\n    end\n\n    def self.size\n      1\n    end\n  end\nend\n",
        ),
        (
            "components/payments/app/services/payments/reader.rb",
            "module Payments\n  class Reader\n    def size\n      Payments::Registry.size\n    end\n  end\nend\n",
        ),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["check", "--continue"])
        .arg(&root)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn a_dropped_files_signatures_do_not_register_its_class() {
    // The file's classes are not in the IR. Its declarations alone used
    // to register `Payments::Registry` with only `label`, so the read
    // reported a missing `size` instead of the ingest gap.
    let text = check_with_dropped_registry();
    assert!(text.contains("error[ingest_unsupported]: class-body local `store`"), "{text}");
    assert!(
        text.contains("dropped by ingest gap: Payments::Registry is defined in"),
        "the reference names the gap:\n{text}"
    );
    assert!(!text.contains("no known method `size`"), "{text}");
}
