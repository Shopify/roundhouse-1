//! Validation for the deliberately small generated-column expression
//! subset that Roundhouse can render to both SQLite and PostgreSQL.
//!
//! Keeping the original SQL on [`super::GeneratedColumn`] is important for
//! schema round-trips. This parser is only a gate: it accepts string
//! literals, references to ordinary text/string columns, `||`, and
//! `coalesce(...)`. It does not rewrite expressions or guess at casts,
//! JSON operators, arbitrary functions, or other database-specific SQL.

use super::{Column, ColumnType, Table};

#[derive(Clone, Debug)]
enum Expr {
    String,
    Column { name: String, quoted: bool },
    Concat(Box<Expr>, Box<Expr>),
    Coalesce(Vec<Expr>),
}

/// Check the supported expression grammar and its referenced columns.
/// The table is complete before this is called, so forward references
/// cannot accidentally be mistaken for an untyped identifier.
pub(crate) fn validate_column(table: &Table, column: &Column) -> Result<(), String> {
    let Some(generated) = &column.generated else {
        return Ok(());
    };

    if column.primary_key {
        return Err("generated columns cannot be primary keys".into());
    }
    if ["id", "created_at", "updated_at"]
        .iter()
        .any(|reserved| column.name.as_str().eq_ignore_ascii_case(reserved))
    {
        return Err(format!(
            "generated column name `{}` conflicts with Roundhouse key/timestamp handling",
            column.name.as_str()
        ));
    }
    if column.default.is_some() {
        return Err("generated columns cannot also have a default".into());
    }
    if column.generated_text_compatible == Some(false) {
        return Err(
            "the original generated-column result type does not have portable text semantics"
                .into(),
        );
    }
    if !is_text_type(&column.col_type) {
        return Err(
            "the supported generated-column result types are unbounded string and text".into(),
        );
    }

    let expression = Parser::new(&generated.expression).parse()?;
    validate_expr(&expression, table)
}

/// All generated-column errors for a table, preserving the column name
/// so callers can ledger every unsupported declaration in survey mode.
pub(crate) fn validate_table(table: &Table) -> Vec<(String, String)> {
    let mut errors: Vec<(String, String)> = table
        .columns
        .iter()
        .filter(|column| column.generated.is_some())
        .filter_map(|column| {
            validate_column(table, column)
                .err()
                .map(|message| (column.name.as_str().to_string(), message))
        })
        .collect();
    if table
        .columns
        .iter()
        .any(|column| column.generated.is_some())
        && table
            .columns
            .iter()
            .all(|column| column.generated.is_some())
    {
        errors.extend(table.columns.iter().map(|column| {
            (
                column.name.as_str().to_string(),
                "tables with generated columns require at least one ordinary column".into(),
            )
        }));
    }
    errors
}

fn is_text_type(col_type: &ColumnType) -> bool {
    matches!(
        col_type,
        ColumnType::Text | ColumnType::String { limit: None }
    )
}

fn validate_expr(expr: &Expr, table: &Table) -> Result<(), String> {
    match expr {
        Expr::String => Ok(()),
        Expr::Column { name, quoted } => {
            let resolved = if *quoted {
                name.clone()
            } else {
                name.to_ascii_lowercase()
            };
            let Some(column) = table
                .columns
                .iter()
                .find(|column| column.name.as_str() == resolved)
            else {
                return Err(format!("expression references unknown column `{resolved}`"));
            };
            if column.generated.is_some() {
                return Err(format!(
                    "expression references generated column `{resolved}`"
                ));
            }
            if column.generated_text_compatible == Some(false) {
                return Err(format!(
                    "expression column `{resolved}` has non-portable text semantics from its original database type"
                ));
            }
            if !is_text_type(&column.col_type) {
                return Err(format!(
                    "expression column `{resolved}` has type {:?}; only text/string operands are supported",
                    column.col_type
                ));
            }
            Ok(())
        }
        Expr::Concat(left, right) => {
            validate_expr(left, table)?;
            validate_expr(right, table)
        }
        Expr::Coalesce(args) => {
            for arg in args {
                validate_expr(arg, table)?;
            }
            Ok(())
        }
    }
}

struct Parser<'a> {
    source: &'a str,
    position: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn parse(mut self) -> Result<Expr, String> {
        if self.source.contains('\0') {
            return Err("NUL bytes in generated expressions are unsupported".into());
        }
        // `structure.sql` is currently decoded with `from_utf8_lossy`
        // before its statement parser runs. Reject its replacement rune
        // so an invalid source byte cannot be retained as if it were an
        // intentional character in a string literal or identifier.
        if self.source.contains('\u{fffd}') {
            return Err(
                "replacement character U+FFFD in generated expressions is unsupported".into(),
            );
        }
        let expression = self.parse_concat()?;
        self.skip_space();
        if self.position != self.source.len() {
            return Err(self.unsupported_at("unsupported SQL syntax"));
        }
        Ok(expression)
    }

    fn parse_concat(&mut self) -> Result<Expr, String> {
        let mut expression = self.parse_atom()?;
        loop {
            self.skip_space();
            if !self.consume("||") {
                return Ok(expression);
            }
            let right = self.parse_atom()?;
            expression = Expr::Concat(Box::new(expression), Box::new(right));
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        self.skip_space();
        match self.peek_char() {
            Some('(') => {
                self.position += 1;
                let expression = self.parse_concat()?;
                self.skip_space();
                if !self.consume(")") {
                    return Err(self.unsupported_at("expected `)`"));
                }
                Ok(expression)
            }
            Some('\'') => self.read_string().map(|()| Expr::String),
            Some('"') => self
                .read_quoted_identifier()
                .map(|name| Expr::Column { name, quoted: true }),
            Some(ch) if is_ident_start(ch) => {
                let name = self.read_identifier();
                self.skip_space();
                if self.peek_char() == Some('(') {
                    if !name.eq_ignore_ascii_case("coalesce") {
                        return Err(format!(
                            "generated expression function `{name}` is unsupported"
                        ));
                    }
                    self.position += 1;
                    self.parse_coalesce()
                } else {
                    if is_sql_keyword(&name) {
                        return Err(format!(
                            "unquoted SQL keyword `{name}` is unsupported in generated expressions"
                        ));
                    }
                    Ok(Expr::Column {
                        name,
                        quoted: false,
                    })
                }
            }
            _ => Err(self.unsupported_at(
                "expected a text column, string literal, or parenthesized expression",
            )),
        }
    }

    fn parse_coalesce(&mut self) -> Result<Expr, String> {
        let mut args = Vec::new();
        loop {
            args.push(self.parse_concat()?);
            self.skip_space();
            if self.consume(",") {
                continue;
            }
            if self.consume(")") {
                break;
            }
            return Err(self.unsupported_at("expected `,` or `)` in coalesce"));
        }
        if args.len() < 2 {
            return Err(
                "coalesce in a generated expression requires at least two arguments".into(),
            );
        }
        Ok(Expr::Coalesce(args))
    }

    fn read_string(&mut self) -> Result<(), String> {
        self.position += 1; // opening apostrophe
        loop {
            let Some(ch) = self.peek_char() else {
                return Err("unterminated string literal in generated expression".into());
            };
            self.position += ch.len_utf8();
            match ch {
                '\'' if self.peek_char() == Some('\'') => {
                    self.position += 1;
                }
                '\'' => return Ok(()),
                '\\' => {
                    return Err(
                        "backslash escapes in generated string literals are unsupported".into(),
                    );
                }
                _ => {}
            }
        }
    }

    fn read_quoted_identifier(&mut self) -> Result<String, String> {
        self.position += 1; // opening double quote
        let mut name = String::new();
        loop {
            let Some(ch) = self.peek_char() else {
                return Err("unterminated quoted identifier in generated expression".into());
            };
            self.position += ch.len_utf8();
            match ch {
                '"' if self.peek_char() == Some('"') => {
                    self.position += 1;
                    name.push('"');
                }
                '"' => {
                    if name.is_empty() {
                        return Err(
                            "empty quoted identifiers are unsupported in generated expressions"
                                .into(),
                        );
                    }
                    return Ok(name);
                }
                _ => name.push(ch),
            }
        }
    }

    fn read_identifier(&mut self) -> String {
        let start = self.position;
        while let Some(ch) = self.peek_char() {
            if !is_ident_continue(ch) {
                break;
            }
            self.position += ch.len_utf8();
        }
        self.source[start..self.position].to_string()
    }

    fn skip_space(&mut self) {
        while let Some(ch) = self.peek_char() {
            if !matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{000c}') {
                break;
            }
            self.position += ch.len_utf8();
        }
    }

    fn consume(&mut self, literal: &str) -> bool {
        if self.source[self.position..].starts_with(literal) {
            self.position += literal.len();
            true
        } else {
            false
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.source.get(self.position..)?.chars().next()
    }

    fn unsupported_at(&self, message: &str) -> String {
        format!("{message} at byte {}", self.position)
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

/// PostgreSQL 18 keyword inventory from the release-pinned parser table:
/// https://github.com/postgres/postgres/blob/REL_18_0/src/include/parser/kwlist.h
/// PostgreSQL's versioned classification is documented at:
/// https://www.postgresql.org/docs/18/sql-keywords-appendix.html
/// This deliberately includes every keyword class (not only reserved words),
/// so a bare column reference must be quoted whenever either target treats
/// it as a keyword. That avoids context-dependent PostgreSQL token handling.
const POSTGRES_18_KEYWORDS: &str = "
abort absent absolute access action add admin after aggregate all
also alter always analyse analyze and any array as asc
asensitive assertion assignment asymmetric at atomic attach attribute authorization backward
before begin between bigint binary bit boolean both breadth by
cache call called cascade cascaded case cast catalog chain char
character characteristics check checkpoint class close cluster coalesce collate collation
column columns comment comments commit committed compression concurrently conditional configuration
conflict connection constraint constraints content continue conversion copy cost create
cross csv cube current current_catalog current_date current_role current_schema current_time current_timestamp
current_user cursor cycle data database day deallocate dec decimal declare
default defaults deferrable deferred definer delete delimiter delimiters depends depth
desc detach dictionary disable discard distinct do document domain double
drop each else empty enable encoding encrypted end enforced enum
error escape event except exclude excluding exclusive execute exists explain
expression extension external extract false family fetch filter finalize first
float following for force foreign format forward freeze from full
function functions generated global grant granted greatest group grouping groups
handler having header hold hour identity if ilike immediate immutable
implicit import in include including increment indent index indexes inherit
inherits initially inline inner inout input insensitive insert instead int
integer intersect interval into invoker is isnull isolation join json
json_array json_arrayagg json_exists json_object json_objectagg json_query json_scalar json_serialize json_table json_value
keep key keys label language large last lateral leading leakproof
least left level like limit listen load local localtime localtimestamp
location lock locked logged mapping match matched materialized maxvalue merge
merge_action method minute minvalue mode month move name names national
natural nchar nested new next nfc nfd nfkc nfkd no
none normalize normalized not nothing notify notnull nowait null nullif
nulls numeric object objects of off offset oids old omit
on only operator option options or order ordinality others out
outer over overlaps overlay overriding owned owner parallel parameter parser
partial partition passing password path period placing plan plans policy
position preceding precision prepare prepared preserve primary prior privileges procedural
procedure procedures program publication quote quotes range read real reassign
recursive ref references referencing refresh reindex relative release rename repeatable
replace replica reset restart restrict return returning returns revoke right
role rollback rollup routine routines row rows rule savepoint scalar
schema schemas scroll search second security select sequence sequences serializable
server session session_user set setof sets share show similar simple
skip smallint snapshot some source sql stable standalone start statement
statistics stdin stdout storage stored strict string strip subscription substring
support symmetric sysid system system_user table tables tablesample tablespace target
temp template temporary text then ties time timestamp to trailing
transaction transform treat trigger trim true truncate trusted type types
uescape unbounded uncommitted unconditional unencrypted union unique unknown unlisten unlogged
until update user using vacuum valid validate validator value values
varchar variadic varying verbose version view views virtual volatile when
where whitespace window with within without work wrapper write xml
xmlattributes xmlconcat xmlelement xmlexists xmlforest xmlnamespaces xmlparse xmlpi xmlroot xmlserialize
xmltable year yes zone
";

/// SQLite's full keyword inventory, including words omitted by builds with
/// optional features disabled, from:
/// https://www.sqlite.org/lang_keywords.html (147-word list, retrieved 2026-10-08).
const SQLITE_KEYWORDS: &str = "
ABORT ACTION ADD AFTER ALL ALTER ALWAYS ANALYZE AND AS
ASC ATTACH AUTOINCREMENT BEFORE BEGIN BETWEEN BY CASCADE CASE CAST
CHECK COLLATE COLUMN COMMIT CONFLICT CONSTRAINT CREATE CROSS CURRENT CURRENT_DATE
CURRENT_TIME CURRENT_TIMESTAMP DATABASE DEFAULT DEFERRABLE DEFERRED DELETE DESC DETACH DISTINCT
DO DROP EACH ELSE END ESCAPE EXCEPT EXCLUDE EXCLUSIVE EXISTS
EXPLAIN FAIL FILTER FIRST FOLLOWING FOR FOREIGN FROM FULL GENERATED
GLOB GROUP GROUPS HAVING IF IGNORE IMMEDIATE IN INDEX INDEXED
INITIALLY INNER INSERT INSTEAD INTERSECT INTO IS ISNULL JOIN KEY
LAST LEFT LIKE LIMIT MATCH MATERIALIZED NATURAL NO NOT NOTHING
NOTNULL NULL NULLS OF OFFSET ON OR ORDER OTHERS OUTER
OVER PARTITION PLAN PRAGMA PRECEDING PRIMARY QUERY RAISE RANGE RECURSIVE
REFERENCES REGEXP REINDEX RELEASE RENAME REPLACE RESTRICT RETURNING RIGHT ROLLBACK
ROW ROWS SAVEPOINT SELECT SET TABLE TEMP TEMPORARY THEN TIES
TO TRANSACTION TRIGGER UNBOUNDED UNION UNIQUE UPDATE USING VACUUM VALUES
VIEW VIRTUAL WHEN WHERE WINDOW WITH WITHOUT
";

/// A bare identifier that is a keyword in either supported DDL dialect must
/// be double-quoted in the retained source expression. We do not rewrite the
/// SQL text, so quoting remains an explicit source-level requirement.
fn is_sql_keyword(name: &str) -> bool {
    POSTGRES_18_KEYWORDS
        .split_ascii_whitespace()
        .chain(SQLITE_KEYWORDS.split_ascii_whitespace())
        .any(|keyword| name.eq_ignore_ascii_case(keyword))
}
