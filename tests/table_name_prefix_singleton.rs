//! A namespace's `table_name_prefix` written inside `class << self`.
//!
//! Every prefix Core declares is in that form (`ShopifyPayments`,
//! `Billing`, `Shipping`, `Checkouts::One`, …). Only `def
//! self.table_name_prefix` was read, so `ShopifyPayments::Account` looked
//! for `accounts`, bound no schema row, and its columns (`shop_id`,
//! `product_key`, …) were `no known method`.

use std::process::Command;

fn check(name: &str, namespace: &str) -> String {
    let root = std::env::temp_dir().join(format!("rh_table_name_prefix_singleton_{}_{name}", std::process::id()));
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n".to_string()),
        ("db/structure.sql", "CREATE TABLE core.billing_charges (\n    id bigint NOT NULL,\n    product_key character varying NOT NULL\n);\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("app/models/billing.rb", namespace.to_string()),
        ("app/models/billing/charge.rb", "module Billing\n  class Charge < ApplicationRecord\n    #: -> untyped\n    def use = product_key.zzz\n  end\nend\n".to_string()),
    ];
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn both_singleton_spellings_prefix_the_table() {
    for (name, namespace) in [
        ("sclass", "module Billing\n  class << self\n    #: -> String\n    def table_name_prefix\n      \"billing_\"\n    end\n  end\nend\n"),
        ("self_def", "module Billing\n  def self.table_name_prefix\n    \"billing_\"\n  end\nend\n"),
    ] {
        let text = check(name, namespace);
        assert!(text.contains("no known method `zzz` on String"), "{name}: {text}");
    }
}

#[test]
fn a_nested_empty_prefix_overrides_the_outer_one() {
    let root = std::env::temp_dir().join(format!("rh_table_name_prefix_singleton_{}_empty", std::process::id()));
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n"),
        ("db/structure.sql", "CREATE TABLE core.vaults (\n    id bigint NOT NULL,\n    label character varying NOT NULL\n);\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/payments.rb", "module Payments\n  class << self\n    def table_name_prefix\n      \"payments_\"\n    end\n  end\nend\n"),
        ("app/models/payments/inner.rb", "module Payments\n  module Inner\n    class << self\n      def table_name_prefix\n        \"\"\n      end\n    end\n  end\nend\n"),
        ("app/models/payments/inner/vault.rb", "module Payments\n  module Inner\n    class Vault < ApplicationRecord\n      #: -> untyped\n      def use = label.yyy\n    end\n  end\nend\n"),
    ];
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(text.contains("no known method `yyy` on String"), "{text}");
}
