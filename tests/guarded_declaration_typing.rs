//! A declaration behind a file-level `return` types as a library class.
//!
//! graphql-shopify's `batch/loader.rb` opens with `begin require
//! "graphql/batch" rescue LoadError; return; end`. The ingest carries the
//! `class Loader < GraphQL::Batch::Loader` after it only in the file's
//! program (`ProgramItem::Inline`), which the analyzer did not read, so
//! the class was unknown and every loader below it lost the RBI's
//! `self.for`. Emit keeps the declaration behind the guard; typing sees
//! it. A declaration that is never reached types too, with nothing
//! invented about it.

use std::process::Command;

const RBI: &str = "module GraphQL; end\nmodule GraphQL::Batch; end\n\nclass GraphQL::Batch::Loader\n  def load(key); end\n\n  class << self\n    def for(*, **, &); end\n  end\nend\n";

const LOADER: &str = "begin\n  require \"graphql/batch\"\nrescue LoadError\n  return\nend\n\nmodule GraphQL\n  module Shopify\n    module Batch\n      class Loader < GraphQL::Batch::Loader\n        #: -> Integer\n        def size = 1\n      end\n    end\n  end\nend\n";

const RECORD_LOADER: &str = "module GraphQL\n  module Shopify\n    module Batch\n      class RecordLoader < GraphQL::Shopify::Batch::Loader\n      end\n    end\n  end\nend\n";

const UNREACHED: &str = "return\n\nclass Unreached\n  #: -> Integer\n  def count = 1\nend\n";

fn check(name: &str, account: &str) -> String {
    let root = std::env::temp_dir().join(format!("rh_guarded_declaration_typing_{}_{name}", std::process::id()));
    let gem = "vendor/gems/fakegql-1.0/lib/fakegql";
    let files = [
        ("roundhouse.yml".to_string(), "test_paths:\n  - test\ngem_sources:\n  fakegql: vendor/gems/fakegql-1.0\n"),
        ("db/structure.sql".to_string(), "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n"),
        ("app/models/application_record.rb".to_string(), "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("sorbet/rbi/gems/graphql-batch@0.6.1.rbi".to_string(), RBI),
        (format!("{gem}/loader.rb"), LOADER),
        (format!("{gem}/record_loader.rb"), RECORD_LOADER),
        (format!("{gem}/unreached.rb"), UNREACHED),
        ("app/models/account.rb".to_string(), account),
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

fn errors(text: &str) -> Vec<&str> {
    text.lines().filter(|line| line.contains(": error[")).collect()
}

#[test]
fn a_subclass_keeps_the_rbi_class_side() {
    let account = "class Account < ApplicationRecord\n  #: -> untyped\n  def use = [GraphQL::Shopify::Batch::RecordLoader.for(1).load(2), GraphQL::Shopify::Batch::RecordLoader.new.size.zzz]\nend\n";
    let text = check("subclass", account);
    assert!(text.contains("no known method `zzz` on Integer"), "{text}");
    assert_eq!(errors(&text).len(), 1, "{text}");
}

#[test]
fn an_unreached_declaration_types_without_an_invented_error() {
    let account = "class Account < ApplicationRecord\n  #: -> untyped\n  def use = Unreached.new.count.yyy\nend\n";
    let text = check("unreached", account);
    assert!(text.contains("no known method `yyy` on Integer"), "{text}");
    assert_eq!(errors(&text).len(), 1, "{text}");
}
