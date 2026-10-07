//! A signature may name a sorbet type alias another file declares.
//!
//! ```ruby
//! # app/models/storefront_carts.rb
//! module StorefrontCarts
//!   OperationResultType = T.type_alias { T::Array[String] }
//! end
//!
//! # app/models/storefront_carts/repository.rb
//! module StorefrontCarts
//!   class Repository
//!     #: (Integer n) -> OperationResultType
//!     def update(n) = ...
//! ```
//!
//! The sig reader resolves only the aliases of its own file, so the name
//! was read as a class (`OperationResultType`, with no methods) and every
//! call on the value was `no known method`. Aliases are found the way
//! `T.let` casts find them, innermost enclosing scope first, across every
//! file, including a module that holds nothing but aliases; the aliased type
//! is read in the scope that declares it.

use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(files: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(format!(
        "rh_cross_file_alias_{}_{}",
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
        (
            "app/models/storefront_carts.rb",
            "# typed: strict\nmodule StorefrontCarts\n  OperationResultType = T.type_alias do\n    T::Array[String]\n  end\nend\n",
        ),
        (
            "app/models/customizations/types.rb",
            "module Customizations\n  module Types\n    ErrorFieldPath = T.type_alias { T::Array[T.any(Symbol, Integer)] }\n  end\nend\n",
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

const REPOSITORY: (&str, &str) = (
    "app/models/storefront_carts/repository.rb",
    r#"module StorefrontCarts
  class Repository
    #: (
    #|   Integer n,
    #|   ?clear: bool
    #| ) -> OperationResultType
    def update(n, clear: false)
      Array.new(n) { "x" }
    end
  end
end
"#,
);

#[test]
fn a_signature_reads_an_alias_declared_in_another_file() {
    let output = check(&[
        REPOSITORY,
        (
            "app/models/customizations/validator.rb",
            r#"module Customizations
  class Validator
    #: (Types::ErrorFieldPath field_path, Integer index) -> Types::ErrorFieldPath
    def child(field_path, index)
      field_path + [index]
    end
  end
end
"#,
        ),
        (
            "app/controllers/carts_controller.rb",
            r#"class CartsController < ApplicationController
  extend T::Sig

  def show
    StorefrontCarts::Repository.new.update(2)
      .each do |line|
        line.upcase
      end
    Customizations::Validator.new.child([:a], 1).first
    head :ok
  end

  private

  #: (StorefrontCarts::OperationResultType result) -> Integer
  def size_of(result)
    result.map(&:upcase).size
  end

  sig { params(result: StorefrontCarts::OperationResultType).returns(T::Boolean) }
  def blank_result?(result)
    result.empty?
  end
end
"#,
        ),
    ]);
    assert!(!output.contains("no known method"), "{output}");
    assert!(!output.contains("incompatible_binop"), "{output}");
}

#[test]
fn the_aliased_type_still_refuses_what_it_lacks() {
    let output = check(&[
        REPOSITORY,
        (
            "app/controllers/carts_controller.rb",
            r#"class CartsController < ApplicationController
  def show
    StorefrontCarts::Repository.new.update(2).imaginary_method
    head :ok
  end
end
"#,
        ),
    ]);
    assert!(output.contains("no known method `imaginary_method` on Array[String]"), "{output}");
}
