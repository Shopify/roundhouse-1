//! String literals whose bytes are not UTF-8, against native Ruby.
//!
//! Core's generated protobuf files (`*_pb.rb`) assign
//! `descriptor_data = "\n\x12\x64\x65vice_token.proto…\xfb\x01…"` and
//! hand it to `DescriptorPool#add_serialized_file`. Ingest read every
//! literal as UTF-8 text, lossily, so each invalid byte was emitted as
//! U+FFFD and the emitted descriptor no longer parsed ("Unable to parse
//! FileDescriptorProto"). The emitted literal must be the same bytes,
//! in the same encoding, frozen where the source froze it: as a
//! constant, a method's return value, a class-body local and a
//! file-level local handed to a call, as the descriptor is, alike.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const DESCRIPTORS: &str = r#"# frozen_string_literal: true

module Descriptors
  DATA = "\n\x12\x64\x65vice_token.proto\"\xfb\x01\n\x0b\x44\x65viceToken\xea\x02$Proto"
  TEXT = "caf\u00e9"
  blob = "\xc3\x28body"
  BLOB = blob

  def self.inline
    "\xff\x00ab"
  end
end
"#;

/// The `_pb.rb` shape: a file-level local the file's program hands on.
const REGISTRY: &str = r#"# frozen_string_literal: true

class DescriptorRegistry
  def self.add(data)
    (@added ||= []) << data
  end

  def self.added = @added
end

descriptor_data = "\n\x12\x64\x65vice.proto\"\xfb\x01\xea\x02$Proto"
DescriptorRegistry.add(descriptor_data)
"#;

/// A US-ASCII source: Ruby gives a `\x` escape outside ASCII there the
/// binary encoding (Prism's forced binary), and nothing is frozen.
const LEGACY: &str = "# encoding: us-ascii\nmodule Legacy\n  MARK = \"\\xff\\xfe\"\nend\n";

/// The probe's own lines, for native Ruby and the emitted tree alike.
const SCRIPT: &str = r#"show = ->(s) { [s.unpack1("H*"), s.encoding.to_s, s.valid_encoding?, s.frozen?] }
puts show.(Descriptors::DATA).inspect
puts show.(Descriptors.inline).inspect
puts show.(Descriptors::BLOB).inspect
puts show.(Legacy::MARK).inspect
puts show.(DescriptorRegistry.added.first).inspect
puts [Descriptors::TEXT, Descriptors::TEXT.encoding.to_s].inspect
"#;

/// What Ruby prints (checked natively), so two equal wrong answers fail.
const EXPECTED: &str = "\
[\"0a126465766963655f746f6b656e2e70726f746f22fb010a0b446576696365546f6b656eea022450726f746f\", \"UTF-8\", false, true]\n\
[\"ff006162\", \"UTF-8\", false, true]\n\
[\"c328626f6479\", \"UTF-8\", false, true]\n\
[\"fffe\", \"ASCII-8BIT\", true, false]\n\
[\"0a126465766963652e70726f746f22fb01ea022450726f746f\", \"UTF-8\", false, true]\n\
[\"café\", \"UTF-8\"]\n";

#[test]
fn a_string_literal_that_is_not_utf8_keeps_its_bytes_as_in_ruby() {
    let dir = std::env::temp_dir().join(format!("rh-binary-literal-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("descriptors.rb"), DESCRIPTORS).unwrap();
    std::fs::write(dir.join("legacy.rb"), LEGACY).unwrap();
    std::fs::write(dir.join("registry.rb"), REGISTRY).unwrap();
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require {:?}\nrequire {:?}\nrequire {:?}\n{SCRIPT}",
            dir.join("descriptors.rb").display().to_string(),
            dir.join("legacy.rb").display().to_string(),
            dir.join("registry.rb").display().to_string()
        ))
        .output()
        .expect("ruby");
    let _ = std::fs::remove_dir_all(&dir);
    let native_out = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(native.status.success(), "native: {native_out}\n{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(native_out, EXPECTED, "native");

    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/descriptors.rb", DESCRIPTORS)
        .write("app/models/legacy.rb", LEGACY)
        .write("app/services/descriptor_registry.rb", REGISTRY)
        .run_ruby(SCRIPT);
    assert!(run.errors.is_empty(), "{:#?}", run.errors);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, native_out, "emitted\n{}", run.stderr);
}
