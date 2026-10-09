//! Which paths are named for secrets (`project::named_for_secrets`): an
//! emitted tree never carries one (docs: BOOT_BOUNDARIES "Secrets files
//! in the emitted tree"). Names are read as whole names and words.
//!
//! The rule matched substrings of every path segment, so the word
//! `token` in Core's `db/data/token_gated_commerce/` withheld
//! `agreement_questions.yml`, the FrozenRecord data
//! `TokenGatedCommerce::AgreementQuestion` reads. unit-1000 r3: 8 rows
//! raised `Errno::ENOENT ./db/data/token_gated_commerce/
//! agreement_questions.yml` and 339 rows' setup raised `NotImplementedError
//! TokenGatedCommerce::AttestationQuestionnaire.table_name must be
//! overridden`, its class body having died at `AgreementQuestion.all`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::named_for_secrets;

#[test]
fn keys_certificates_encrypted_credentials_and_secrets_files_are_named_for_secrets() {
    for path in [
        "config/credentials/test.key",
        "x.pem",
        "secrets.ejson",
        "master.key",
        "config/master.key",
        "credentials.yml.enc",
        "config/credentials/production.yml.enc",
        "config/credentials.yml",
        "config/credentials/test.yml",
        "config/SECRETS/data.yml",
        "config/secrets.test.json",
        "config/secrets.development.json",
        "config/api_client_secrets.yml",
        "config/api_token.yml",
        "db/data/passwords.json",
        "config/private_keys.yml",
        "config/signing_key.json",
        "certs/id_ed25519",
        "certs/store.p12",
        // A key is a key whatever its directory or case.
        "db/data/token_gated_commerce/MASTER.KEY",
    ] {
        assert!(named_for_secrets(path), "{path} should be named for secrets");
    }
}

#[test]
fn a_word_in_a_directory_name_or_a_modifier_names_no_secret() {
    for path in [
        "db/data/token_gated_commerce/agreement_questions.yml",
        "app/models/access_token.rb",
        "app/models/token_gated_commerce/attestation_questionnaire.rb",
        // Core's taxes FrozenRecord data and its test data.
        "components/taxes/db/taxes/filing/credential_rulesets.yml",
        "components/taxes/test/fixtures/taxes/filing/credential_rulesets_tests/tn_test.json",
        "components/home/config/locales/task_definitions/remove_password_task/en.yml",
        "test/data/payments/stripe_intent/receipt_stripe_tokenization.json",
        "test/data/shopify_payments/kyc/legal_entities/hongkong_private_partnership.json",
        "config/translation_keys.yml",
        "config/three_d_secure.yml",
        "db/data/apps/private_token_ownerships.yml",
    ] {
        assert!(!named_for_secrets(path), "{path} is not named for secrets");
    }
}

/// The emitted tree carries the data under a directory whose name has a
/// secrets word, and a class body reads it, while a key and encrypted
/// credentials beside it stay out.
#[test]
fn data_under_a_token_named_directory_is_carried_and_keys_are_not() {
    let run = emit_and_run::empty_app()
        .write("db/data/token_gated_commerce/agreement_questions.yml", "- id: 1\n  text: Do you agree?\n")
        .write("config/credentials/test.key", "0123456789abcdef0123456789abcdef\n")
        .write("config/credentials/test.yml.enc", "not-a-real-ciphertext\n")
        .write("config/master.key", "0123456789abcdef0123456789abcdef\n")
        .write(
            "lib/agreement_questions.rb",
            "require \"yaml\"\n\nclass AgreementQuestions\n  ALL = YAML.safe_load_file(File.join(Rails.root, \"db\", \"data\", \"token_gated_commerce\", \"agreement_questions.yml\")).freeze\nend\n",
        )
        .run_ruby("p AgreementQuestions::ALL\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[{\"id\" => 1, \"text\" => \"Do you agree?\"}]\n");
    assert!(run.emitted.join("db/data/token_gated_commerce/agreement_questions.yml").is_file());
    for path in ["config/credentials/test.key", "config/credentials/test.yml.enc", "config/credentials", "config/master.key"] {
        assert!(!run.emitted.join(path).exists(), "{path} was carried");
    }
}
