//! Composite association keys: `foreign_key: [:shop_id, :order_id]`,
//! `primary_key: [..]`, `query_constraints: [..]`, and Core's
//! `yugabyte_overrides: { query_constraints:, primary_key:, foreign_key: }`.
//!
//! Rails (the Shopify fork Core runs on) reads every such option as a list
//! of column PAIRS: the column on the foreign-key side zipped with the
//! column on the referenced side (`has_one`/`has_many`: target column with
//! owner column; `belongs_to`: owner column with target column). The IR
//! keeps one pair as the association's `foreign_key` / `primary_key` and the
//! rest as `extra_keys`, so every single-column consumer keeps working and
//! only the readers that know about `extra_keys` add the other equalities.
//!
//! Core declares each key twice: the plain options MySQL reads, and
//! `yugabyte_overrides` that `YugabyteAssociations` swaps in when
//! `yugabyte_testing?` (`DB_ADAPTER=postgresql`, which is every run here).
//! The wrapper drops the plain `primary_key`/`foreign_key`/`query_constraints`
//! and forwards the overrides in their place, so the overrides win whenever
//! they are written.

/// One key option as written: a column, or a literal list of columns.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KeyOption {
    pub columns: Vec<String>,
    pub array: bool,
}

/// The key-bearing options of one declaration.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KeyOptions {
    pub foreign_key: Option<KeyOption>,
    pub primary_key: Option<KeyOption>,
    pub query_constraints: Option<KeyOption>,
}

/// The pair the association's single-column fields carry, plus the rest.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedKeys {
    /// The writable foreign-key column.
    pub foreign_key: String,
    /// The referenced-side column paired with `foreign_key`; `None` is `id`.
    pub primary_key: Option<String>,
    /// Every other `(foreign-key side, referenced side)` pair.
    pub extra_keys: Vec<(String, String)>,
}

/// Resolve the declaration's key options. `None` means a single-column key
/// the existing `foreign_key` / `primary_key` fields already describe.
pub(crate) fn resolve(plain: &KeyOptions, overrides: Option<&KeyOptions>) -> Option<ResolvedKeys> {
    let opts = overrides.unwrap_or(plain);
    let fk = opts.foreign_key.as_ref();
    let pk = opts.primary_key.as_ref();

    // `query_constraints` + `primary_key` lists: the pairs are the lists
    // zipped, and `foreign_key` names the writable one (the wrapper's
    // `has_one_with_query_constraints` / `belongs_to_with_query_constraints`).
    // A bare `query_constraints:` beside a scalar key (native Rails) is
    // additive: each named column is compared with itself.
    if let Some(qc) = &opts.query_constraints {
        let pairs: Vec<(String, String)> = match pk {
            Some(pk) if pk.array && pk.columns.len() == qc.columns.len() => {
                qc.columns.iter().cloned().zip(pk.columns.iter().cloned()).collect()
            }
            _ => {
                let mut p = vec![];
                let writable = fk.and_then(|f| f.columns.first().cloned());
                let parent = pk.and_then(|p| p.columns.first().cloned()).unwrap_or_else(|| "id".into());
                if let Some(w) = &writable {
                    p.push((w.clone(), parent));
                }
                p.extend(qc.columns.iter().filter(|c| Some(*c) != writable.as_ref()).map(|c| (c.clone(), c.clone())));
                p
            }
        };
        // A list names every writable column (`foreign_key: [:contract_identity_id,
        // :shop_id]`); the primary pair is the one meeting the referenced `id`,
        // as without `query_constraints`, not merely the last column listed.
        let writable = fk.and_then(|f| {
            let meets_id = (f.columns.len() > 1)
                .then(|| pairs.iter().find(|(c, parent)| parent == "id" && f.columns.contains(c)))
                .flatten()
                .map(|(c, _)| c.clone());
            meets_id.or_else(|| f.columns.last().cloned())
        });
        return split(pairs, writable.as_deref());
    }

    let fk = fk?;
    if !fk.array {
        return None;
    }
    let pairs: Vec<(String, String)> = match pk {
        Some(pk) if pk.array && pk.columns.len() == fk.columns.len() => {
            fk.columns.iter().cloned().zip(pk.columns.iter().cloned()).collect()
        }
        _ => {
            // `foreign_key: [:shop_id, :order_id]` against the owner's own
            // `[:shop_id, :id]` key: the trailing non-tenant column is the
            // real foreign key and meets the key's `id` (or the scalar
            // `primary_key:`); the others meet their own name.
            let writable = fk.columns.iter().rev().find(|c| c.as_str() != "shop_id").or(fk.columns.last())?.clone();
            let parent = pk.and_then(|p| p.columns.first().cloned()).unwrap_or_else(|| "id".into());
            fk.columns
                .iter()
                .map(|c| if *c == writable { (c.clone(), parent.clone()) } else { (c.clone(), c.clone()) })
                .collect()
        }
    };
    // Without a `query_constraints` split every column is a foreign key; the
    // primary pair is the one meeting the referenced `id`, else the last.
    let writable = pairs.iter().find(|(_, parent)| parent == "id").or(pairs.last()).map(|(c, _)| c.clone());
    split(pairs, writable.as_deref())
}

/// Whether the declaration's `query_constraints` columns beyond the writable
/// `foreign_key` are left as the owner has them. A `foreign_key` list that
/// covers every constraint column (`query_constraints: [:contract_identity_id,
/// :shop_id], foreign_key: [:contract_identity_id, :shop_id]`) leaves none:
/// `replace_keys` writes every part.
pub(crate) fn constraint_keys_preserved(opts: &KeyOptions) -> bool {
    let Some(qc) = &opts.query_constraints else {
        return false;
    };
    match &opts.foreign_key {
        Some(fk) if fk.array => !qc.columns.iter().all(|c| fk.columns.contains(c)),
        _ => true,
    }
}

fn split(pairs: Vec<(String, String)>, writable: Option<&str>) -> Option<ResolvedKeys> {
    let primary = writable
        .and_then(|w| pairs.iter().position(|(c, _)| c == w))
        .or(if pairs.is_empty() { None } else { Some(0) })?;
    let (foreign_key, parent) = pairs[primary].clone();
    let extra_keys = pairs.into_iter().enumerate().filter(|(i, _)| *i != primary).map(|(_, p)| p).collect();
    Some(ResolvedKeys {
        foreign_key,
        primary_key: (parent != "id").then_some(parent),
        extra_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(c: &str) -> Option<KeyOption> {
        Some(KeyOption { columns: vec![c.into()], array: false })
    }
    fn list(cs: &[&str]) -> Option<KeyOption> {
        Some(KeyOption { columns: cs.iter().map(|c| c.to_string()).collect(), array: true })
    }
    fn pairs(ps: &[(&str, &str)]) -> Vec<(String, String)> {
        ps.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn scalar_key_is_not_composite() {
        let plain = KeyOptions { foreign_key: scalar("order_id"), ..Default::default() };
        assert_eq!(resolve(&plain, None), None);
    }

    #[test]
    fn yugabyte_overrides_win_and_zip_query_constraints_with_primary_key() {
        // Order#draft_order
        let plain = KeyOptions { foreign_key: list(&["shop_id", "active_order_id"]), ..Default::default() };
        let over = KeyOptions {
            query_constraints: list(&["active_order_id", "shop_id"]),
            primary_key: list(&["id", "shop_id"]),
            foreign_key: scalar("active_order_id"),
        };
        let r = resolve(&plain, Some(&over)).unwrap();
        assert_eq!(r.foreign_key, "active_order_id");
        assert_eq!(r.primary_key, None);
        assert_eq!(r.extra_keys, pairs(&[("shop_id", "shop_id")]));
    }

    #[test]
    fn overrides_keep_a_non_id_primary_key_for_the_writable_column() {
        // `query_constraints: [:shop_id, :user_id], primary_key: [:shop_id, :id]`
        let over = KeyOptions {
            query_constraints: list(&["shop_id", "user_id"]),
            primary_key: list(&["shop_id", "id"]),
            foreign_key: scalar("user_id"),
        };
        let r = resolve(&KeyOptions::default(), Some(&over)).unwrap();
        assert_eq!((r.foreign_key.as_str(), r.primary_key), ("user_id", None));
        assert_eq!(r.extra_keys, pairs(&[("shop_id", "shop_id")]));
    }

    #[test]
    fn a_foreign_key_list_covering_the_constraints_writes_every_part() {
        // ContractDiscount#contract_identity
        let over = KeyOptions {
            query_constraints: list(&["contract_identity_id", "shop_id"]),
            primary_key: list(&["id", "shop_id"]),
            foreign_key: list(&["contract_identity_id", "shop_id"]),
        };
        let r = resolve(&KeyOptions::default(), Some(&over)).unwrap();
        assert_eq!((r.foreign_key.as_str(), r.primary_key), ("contract_identity_id", None));
        assert_eq!(r.extra_keys, pairs(&[("shop_id", "shop_id")]));
        assert!(!constraint_keys_preserved(&over));
        // A scalar writable key leaves the other constraint columns alone.
        let scalar_fk = KeyOptions { foreign_key: scalar("contract_identity_id"), ..over };
        assert!(constraint_keys_preserved(&scalar_fk));
    }

    #[test]
    fn plain_array_foreign_key_meets_the_owners_id() {
        let plain = KeyOptions { foreign_key: list(&["shop_id", "order_id"]), ..Default::default() };
        let r = resolve(&plain, None).unwrap();
        assert_eq!(r.foreign_key, "order_id");
        assert_eq!(r.primary_key, None);
        assert_eq!(r.extra_keys, pairs(&[("shop_id", "shop_id")]));
    }

    #[test]
    fn explicit_array_primary_key_pairs_positionally() {
        let plain = KeyOptions {
            foreign_key: list(&["shop_id", "inventory_item_id", "inventory_group_id"]),
            primary_key: list(&["shop_id", "inventory_item_id", "inventory_group_id"]),
            ..Default::default()
        };
        let r = resolve(&plain, None).unwrap();
        // No pair meets `id`: the last is the primary one.
        assert_eq!(r.foreign_key, "inventory_group_id");
        assert_eq!(r.primary_key.as_deref(), Some("inventory_group_id"));
        assert_eq!(
            r.extra_keys,
            pairs(&[("shop_id", "shop_id"), ("inventory_item_id", "inventory_item_id")])
        );
    }

    #[test]
    fn native_query_constraints_beside_a_scalar_key_are_additive() {
        let plain = KeyOptions {
            foreign_key: scalar("customer_id"),
            query_constraints: list(&["shop_id"]),
            ..Default::default()
        };
        let r = resolve(&plain, None).unwrap();
        assert_eq!((r.foreign_key.as_str(), r.primary_key), ("customer_id", None));
        assert_eq!(r.extra_keys, pairs(&[("shop_id", "shop_id")]));
    }
}
