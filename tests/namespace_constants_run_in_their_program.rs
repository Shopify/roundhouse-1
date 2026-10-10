//! Namespace constants several file programs write, each after
//! statements of its own (e2e round 7). Core's
//! lib/shopify/protobuf/*_pb.rb files each run
//!
//! ```ruby
//! pool = ::Google::Protobuf::DescriptorPool.generated_pool
//! pool.add_serialized_file(descriptor_data)
//!
//! module Shopify
//!   module Protobuf
//!     Metafield = ::Google::Protobuf::DescriptorPool.generated_pool.lookup("shopify.protobuf.Metafield").msgclass
//! ```
//!
//! Ingest merges every reopen of `Shopify::Protobuf` into one, so the
//! namespace's file held all 123 lookups. The first program to load it
//! had added only its own descriptor, and the others' lookups answered
//! nil: NoMethodError (msgclass for nil) at load. A constant a file
//! program writes after a statement of its own now runs in that
//! program, where the source writes it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const REGISTRY: &str = r##"module ProbePool
  def self.add(name, value)
    (@entries ||= {})[name] = value
  end

  def self.lookup(name)
    (@entries ||= {})[name]
  end
end
"##;

const ALPHA: &str = r##"require_relative "../probe_pool"

pool = ProbePool
pool.add("alpha", "alpha message")

module ProbeProto
  Alpha = ProbePool.lookup("alpha").upcase
end
"##;

const BETA: &str = r##"require_relative "../probe_pool"

ProbePool.add("beta", "beta message")

module ProbeProto
  Beta = ProbePool.lookup("beta").upcase
  BETA_LENGTH = Beta.length
end
"##;

const ENTRY: &str = r##"require_relative "probe_proto/alpha_pb"
require_relative "probe_proto/beta_pb"

module ProbeProto
  def self.names
    [Alpha, Beta]
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::real_blog()
        .write("lib/probe_pool.rb", REGISTRY)
        .write("lib/probe_proto/alpha_pb.rb", ALPHA)
        .write("lib/probe_proto/beta_pb.rb", BETA)
        .write("lib/probe_proto.rb", ENTRY)
}

#[test]
fn each_file_programs_namespace_constants_run_after_its_own_statements() {
    let run = app().run_ruby("p ProbeProto.names\np ProbeProto::BETA_LENGTH\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        "[\"ALPHA MESSAGE\", \"BETA MESSAGE\"]\n12\n",
        "{}",
        run.stderr
    );
}
