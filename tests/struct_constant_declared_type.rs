//! A `#:` ascription naming a class built by `Struct.new` and assigned to a
//! constant of the enclosing class (Core's
//! `OnlineStore::SemanticSearchSettings::Message`) is a class, not a missing
//! declaration: the value it annotates runs.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn an_ascription_naming_a_struct_constant_of_the_class_runs() {
    emit_and_run::real_blog()
        .write(
            "app/services/search_settings.rb",
            r#"module OnlineStore
  class SearchSettings
    Message = Struct.new(:code, :summary, :metadata, keyword_init: true) do
      self::CODES = {
        incomplete: "incomplete",
      }.freeze #: Hash[Symbol, String]

      self::Metadata = Struct.new(:min, keyword_init: true)
    end

    attr_reader :messages

    #: -> void
    def initialize
      @messages = [] #: Array[Message]
      @unmet = [] #: Array[Message]
    end

    #: -> void
    def add
      @messages << Message.new(code: Message::CODES[:incomplete], summary: "x", metadata: Message::Metadata.new(min: 1))
    end
  end
end
"#,
        )
        .run_ruby(
            r#"settings = OnlineStore::SearchSettings.new
settings.add
raise "message lost" unless settings.messages.first.code == "incomplete"
raise "metadata lost" unless settings.messages.first.metadata.min == 1
# The block's `self::CODES` is the Struct's own constant, not the enclosing class's.
raise "leaked to the enclosing class" if OnlineStore::SearchSettings.const_defined?(:CODES, false)
raise "own constant lost" unless OnlineStore::SearchSettings::Message.const_defined?(:CODES, false)
"#,
        )
        .assert_passes();
}

#[test]
fn a_struct_block_writing_constants_through_self_at_file_level_runs() {
    emit_and_run::real_blog()
        .write(
            "app/services/top_message.rb",
            r#"TopMessage = Struct.new(:code, keyword_init: true) do
  self::CODES = {
    incomplete: "incomplete",
  }.freeze #: Hash[Symbol, String]
end
"#,
        )
        .run_ruby(
            r#"raise "lost" unless TopMessage.new(code: TopMessage::CODES[:incomplete]).code == "incomplete"
"#,
        )
        .assert_passes();
}
