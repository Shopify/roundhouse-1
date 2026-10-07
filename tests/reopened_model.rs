//! A model reopened in a second file with its superclass.
//!
//! Core nests services under a model by reopening it
//! (`class AssetPack < CoreGeneral2Record; class InspirationImageIngest`).
//! Ingest records each file as a model, and the last one registered
//! replaced the first: `serialize :payload` was lost, so `payload.merge!`
//! was refused on the column's `String?`, and so were the first file's
//! associations. Both files' declarations now type the class.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const SCHEMA: &str = "CREATE TABLE core.things (\n    id bigint NOT NULL,\n    order_id bigint,\n    payload character varying(65535)\n);\nCREATE TABLE core.orders (\n    id bigint NOT NULL\n);\nCREATE TABLE core.parts (\n    id bigint NOT NULL,\n    thing_id bigint\n);\n";

fn check(model: &str, reopening: &str) -> (bool, String) {
    let root = std::env::temp_dir().join(format!("rh_reopened_model_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n"),
        ("db/structure.sql", SCHEMA),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/order.rb", "class Order < ApplicationRecord\nend\n"),
        ("app/models/part.rb", "class Part < ApplicationRecord\nend\n"),
        ("app/models/thing.rb", model),
        ("app/services/thing/helper.rb", reopening),
    ];
    for (path, text) in &files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    (output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

const NESTS_A_SERVICE: &str = "class Thing < ApplicationRecord\n  class Helper\n  end\nend\n";

#[test]
fn the_first_files_serialize_survives_the_reopening() {
    let model = "class Thing < ApplicationRecord\n  serialize :payload, type: Hash, coder: JSON\n\n  #: -> void\n  def stamp\n    payload.merge!(\"at\" => 1)\n  end\nend\n";
    let (ok, text) = check(model, NESTS_A_SERVICE);
    assert!(ok, "{text}");
}

#[test]
fn the_first_files_association_survives_the_reopening() {
    let model = "class Thing < ApplicationRecord\n  belongs_to :order, optional: true\n\n  #: -> void\n  def touch_order\n    order&.touch\n  end\nend\n";
    let (ok, text) = check(model, NESTS_A_SERVICE);
    assert!(ok, "{text}");
}

#[test]
fn the_reopenings_own_association_types_too() {
    let model = "class Thing < ApplicationRecord\n  #: -> Integer\n  def part_count = parts.size\nend\n";
    let reopening = "class Thing < ApplicationRecord\n  has_many :parts\n\n  class Helper\n  end\nend\n";
    let (ok, text) = check(model, reopening);
    assert!(ok, "{text}");
}
