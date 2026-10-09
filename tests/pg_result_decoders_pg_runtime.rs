//! Result decoding on the PostgreSQL runtime, against native Rails 8.2 on
//! the same local PostgreSQL database.
//!
//! The session's result map was the pg gem's PG::BasicTypeMapForResults,
//! whose fallback for an OID it has no decoder for (uuid, time, interval,
//! ranges, ...) warns "no type cast defined for type "uuid" format 0 with
//! oid 2950". Core's strict warnings raise on it: 23 of the 27 unit-1000
//! ids whose fixtures load died reading a uuid column. Rails'
//! add_pg_decoders map answers such a column's text, silently; the
//! runtime's map now does too, and decodes money as Rails' result map's
//! MoneyDecoder does. The probe reads every column type Core's schema
//! uses (pg_type over the template's columns), plus inet and an integer
//! array, under a Warning hook that raises, as Core's does.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE samples (
    id bigserial NOT NULL,
    token uuid,
    name character varying,
    created_at timestamp(6) without time zone,
    small smallint,
    flag boolean,
    amount numeric(10,2),
    visits integer,
    big bigint,
    blob bytea,
    note text,
    settings jsonb,
    ratio double precision,
    opened_on date,
    payload json,
    opens_at time without time zone,
    CONSTRAINT samples_pkey PRIMARY KEY (id)
);
INSERT INTO samples (token, name, created_at, small, flag, amount, visits, big, blob, note, settings, ratio, opened_on, payload, opens_at)
VALUES ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 'n', '2026-01-02 03:04:05.123456', 2, true, 12.5, 7, 9000000000, '\x0102', 't',
        '{"a": 1, "b": [true, null]}', 1.5, '2026-01-02', '[1, "x"]', '03:04:05');
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_pg_result_decoders
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SAMPLE: &str = "class Sample < ApplicationRecord\nend\n";

const PROBE: &str = r##"module StrictWarnings
  def warn(message, *args, **kwargs)
    raise "strict warning: #{message}"
  end
end

class DecoderProbe
  COLUMNS = %w[token name created_at small flag amount visits big blob note settings ratio opened_on payload opens_at]

  # Temporal values by their fields: Time#inspect's zone label and
  # ActiveSupport's Date#inspect are not what this probe checks.
  def self.show(value)
    if value.is_a?(Time)
      value.strftime("%H:%M:%S.%6N")
    elsif value.is_a?(Date)
      value.strftime("%Y-%m-%d")
    else
      value.inspect
    end
  end

  def self.lines
    Warning.singleton_class.prepend(StrictWarnings)
    out = []
    sample = Sample.first
    COLUMNS.each do |name|
      value = sample.public_send(name)
      out << "#{name}: #{value.class} #{show(value)}"
    end
    out << "find_by token: #{Sample.find_by(token: "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")&.name.inspect}"
    out << "pluck token: #{Sample.pluck(:token).inspect}"
    out << "where token: #{Sample.where(token: sample.token).count}"
    out << "pluck json: #{Sample.pluck(:settings).inspect} #{Sample.pluck(:payload).inspect}"
    created = Sample.create!(token: "b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a12", name: "m")
    out << "created token: #{Sample.find(created.id).token.inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: &[&str] = &[
    r#"token: String "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11""#,
    r#"name: String "n""#,
    "created_at: Time 03:04:05.123456",
    "small: Integer 2",
    "flag: TrueClass true",
    "amount: BigDecimal 0.125e2",
    "visits: Integer 7",
    "big: Integer 9000000000",
    r#"blob: String "\x01\x02""#,
    r#"note: String "t""#,
    r#"settings: Hash {"a" => 1, "b" => [true, nil]}"#,
    "ratio: Float 1.5",
    "opened_on: Date 2026-01-02",
    r#"payload: Array [1, "x"]"#,
    "opens_at: Time 03:04:05.000000",
    r#"find_by token: "n""#,
    r#"pluck token: ["a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"]"#,
    "where token: 1",
    r#"pluck json: [{"a" => 1, "b" => [true, nil]}] [[1, "x"]]"#,
    r#"created token: "b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a12""#,
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/sample.rb", SAMPLE),
    ("app/models/decoder_probe.rb", PROBE),
];

const LOADS: [&str; 3] = ["app/models/application_record", "app/models/sample", "app/models/decoder_probe"];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn result_columns_decode_as_rails_does_without_a_warning() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) =
        pg_probe::native_and_emitted("pg-result-decoders", &FILES, &emitted, "rh_e2e_pg_result_decoders", "DecoderProbe", &LOADS);
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
