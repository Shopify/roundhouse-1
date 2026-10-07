//! A signature may name a constant that holds a class.
//!
//! ```ruby
//! # components/platform/essentials/app/utils/results/result.rb
//! module Results
//!   Result = ::Shopify::Adt::Result
//! end
//!
//! module StorefrontCarts
//!   OperationResultType = T.type_alias { Results::Result[OperationResult, T::Array[Error]] }
//! end
//! ```
//!
//! `Results::Result` is the same module object as `Shopify::Adt::Result`
//! (native: `Results::Result.equal?(Shopify::Adt::Result)` under
//! shopify-adt 1.2.0, which loads without Rails). The signature reader took
//! the name for a class of its own, without the cataloged Result surface,
//! so every `.ok?` / `.ok_value` / `.error_value` on an operation's answer
//! failed (about 96 Core sites once cross-file aliases expanded). A
//! constant whose value is a bare reference to a registered class now reads
//! as that class, keeping the written type arguments.

use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(files: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(format!(
        "rh_class_constant_alias_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let base = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        ),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\n  get \"/things\", to: \"things#show\"\nend\n"),
        (
            "app/utils/results/result.rb",
            "# typed: strict\n# frozen_string_literal: true\n\nmodule Results\n  Result = ::Shopify::Adt::Result\nend\n",
        ),
        (
            "app/models/geometry.rb",
            "module Geometry\n  class Circle\n    def area = 3\n  end\n  Shape = Circle\nend\n",
        ),
    ];
    for (path, text) in base.iter().chain(files) {
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

const OPERATIONS: (&str, &str) = (
    "app/models/storefront_carts.rb",
    r#"# typed: strict
module StorefrontCarts
  OperationResultType = T.type_alias do
    Results::Result[String, T::Array[Symbol]]
  end

  class Repository
    #: (Integer n) -> OperationResultType
    def create(n)
      n > 0 ? Results::Result.ok("yes") : Results::Result.error([:no])
    end

    #: (Integer n) -> Results::Result[String, Symbol]
    def self.perform(n)
      n > 0 ? Results::Result.ok("yes") : Results::Result.error(:no)
    end

    #: -> Geometry::Shape
    def shape = Geometry::Circle.new
  end
end
"#,
);

#[test]
fn a_signature_naming_a_class_constant_reads_that_class() {
    let output = check(&[
        OPERATIONS,
        (
            "app/controllers/things_controller.rb",
            r#"class ThingsController < ApplicationController
  def show
    result = StorefrontCarts::Repository.new.create(1)
    @value = result.ok? ? result.ok_value : result.error_value
    performed = StorefrontCarts::Repository.perform(2)
    @other = performed.ok_value_or("none") if performed.error?
    @area = StorefrontCarts::Repository.new.shape.area
    cast = nil #: Results::Result[String, Symbol]?
    @cast = cast&.ok?
    head :ok
  end
end
"#,
        ),
    ]);
    assert!(!output.contains("no known method"), "{output}");
}

#[test]
fn the_aliased_class_still_refuses_what_it_lacks() {
    let output = check(&[
        OPERATIONS,
        (
            "app/controllers/things_controller.rb",
            r#"class ThingsController < ApplicationController
  def show
    StorefrontCarts::Repository.new.shape.perimeter
    head :ok
  end
end
"#,
        ),
    ]);
    assert!(output.contains("no known method `perimeter` on Geometry::Circle"), "{output}");
}
