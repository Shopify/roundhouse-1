//! An initializer's `require` of a path gem's lib file that loads more
//! files by a glob, as Core's components/checkouts/one protobuf.rb
//! initializer does (`require "checkout_protobuf_definitions"`, whose file
//! runs `Kernel.require(file)` over every `*_pb.rb` beside it).
//!
//! The initializer's require was refused (`loads …, which boot does not
//! load`) and the loader's `Kernel.require` loop ran as written, globbing
//! the working directory: no `_pb.rb` program ran, so
//! `CheckoutProtobufDefinitions::PurchaseOrders::CustomFields` was
//! undefined where custom_fields_codec.rb named it. That NameError aborted
//! cart_next.rb's load inside a test hook, and every unit-1000 setup then
//! raised `uninitialized constant ...::CartNext`.
//!
//! Now the require loads the emitted program, and the loop is the
//! requires of the files it matches (`autoload_glob::file_glob_requires`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOADER: &str = "Dir.glob(File.join(__dir__, \"protos\", \"**\", \"*_pb.rb\")).each do |file|\n  Kernel.require(file)\nend\n";
const ORDER_PB: &str = "require \"json\"\n\nmodule Protos\n  module Orders\n    Fields = ::JSON.parse('[\"alpha\",\"beta\"]').freeze\n  end\nend\n";
const CODEC: &str = "class OrderCodec\n  NAMES = Protos::Orders::Fields\nend\n";
/// Boot (the initializer) loaded the `_pb.rb` file, before anything names its constant.
const SCRIPT: &str = "p Object.const_defined?(:Protos, false) && Protos.const_defined?(:Orders, false) && Protos::Orders.const_defined?(:Fields, false)\np OrderCodec::NAMES\n";

const LOCK: &str = "PATH\n  remote: components/protos\n  specs:\n    protos (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n\nDEPENDENCIES\n  protos!\n";
const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n\ndevelopment:\n  <<: *default\n  database: protos_probe\n\ntest:\n  <<: *default\n  database: protos_probe\n";

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/database.yml", DATABASE_YML)
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"protos\", path: \"components/protos\"\n")
        .write("components/protos/protos.gemspec", "Gem::Specification.new do |s|\n  s.name = \"protos\"\n  s.version = \"0.1.0\"\nend\n")
        .write("components/protos/lib/protos.rb", LOADER)
        .write("components/protos/lib/protos/orders/order_pb.rb", ORDER_PB)
        .write("config/initializers/protos.rb", "require \"protos\"\n")
        .write("app/models/order_codec.rb", CODEC)
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
}

fn read(out: &std::path::Path, rel: &str) -> String {
    std::fs::read_to_string(out.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn the_initializer_loads_the_program_and_the_loop_requires_the_matched_files() {
    let (out, errors) = app().emit(roundhouse::project::BuildTarget::Ruby);
    let holder = read(&out, "app/models/roundhouse_initializer_config_initializers_protos.rb");
    assert!(holder.contains("require_relative \"roundhouse_files/components/protos/lib/protos\""), "{holder}\n{errors:#?}");
    let loader = read(&out, "app/models/roundhouse_files/components/protos/lib/protos.rb");
    assert!(loader.contains("orders/order_pb"), "{loader}");
    assert!(!loader.contains("Dir.glob"), "{loader}");
}

/// The emitted boot. It loads the `pg` gem (it never connects).
#[test]
#[ignore = "requires ruby with the pg gem"]
fn the_emitted_boot_defines_the_constant_the_glob_loaded_file_assigns() {
    let run = app().run_ruby(SCRIPT);
    assert!(run.success, "{}\n=== stdout ===\n{}\n=== stderr ===\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, "true\n[\"alpha\", \"beta\"]\n", "{}", run.stderr);
}
