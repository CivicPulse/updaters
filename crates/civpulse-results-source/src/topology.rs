//! Reviewed public identity catalog; results and labels never enter admission membership.
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};
pub(crate) type Membership = Vec<(String, String, Vec<(String, Vec<String>)>)>;
const CATALOG: &[u8] = include_bytes!("../catalogs/georgia-2026-11-03-v1.json");
const CATALOG_SHA256: &str = "e3a380f76800cae26a72593cc33db5c7f354415c83ab1bff0ea563a27fdefb4b";
const STATE: &str = "09378a07-e6cf-4f66-be7c-ca4aa534f99a";
const EXCEPTIONS: [(&str, &str); 3] = [
    ("SHD39", "YashicaMarshall-86-"),
    ("SHD134", "EugeneAllison-46-"),
    ("SHD180", "DefonsioDaniels-50-"),
];
/// Explicit reviewed source-membership policy, independent of semantic fingerprinting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopologyPolicy {
    Georgia20261103V1,
}
impl TopologyPolicy {
    /// Parse the opt-in policy name; empty preserves legacy admission.
    /// # Errors
    /// Rejects unknown policy identifiers.
    pub fn parse(value: &str) -> Result<Option<Self>> {
        match value {
            "" => Ok(None),
            "georgia-2026-11-03-v1" => Ok(Some(Self::Georgia20261103V1)),
            _ => Err(Error::Invalid("unknown source membership policy")),
        }
    }
    /// The policy applies only to this reviewed official source URL.
    #[must_use]
    pub fn source_url(self) -> &'static str {
        "https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json"
    }
    /// Verify the compiled catalog hash and identity structure before HTTP or parsing.
    /// # Errors
    /// Rejects a modified or malformed bundled catalog.
    pub fn validate_catalog(self) -> Result<()> {
        catalog().map(|_| ())
    }
    pub(crate) fn admit(self, actual: &Membership) -> Result<()> {
        let expected = match self {
            Self::Georgia20261103V1 => catalog()?,
        };
        if actual.len() != expected.len() {
            return Err(Error::Invalid("source membership policy mismatch"));
        }
        for ((role, scope, items), (known_role, known_scope, known_items)) in
            actual.iter().zip(expected)
        {
            if role != known_role || scope != known_scope {
                return Err(Error::Invalid("source membership policy mismatch"));
            }
            let current: BTreeMap<_, _> = items
                .iter()
                .map(|(id, options)| (id.as_str(), options))
                .collect();
            let known: BTreeMap<_, _> = known_items
                .iter()
                .map(|(id, options)| (id.as_str(), options))
                .collect();
            if current.keys().any(|id| !known.contains_key(id)) {
                return Err(Error::Invalid("source membership policy mismatch"));
            }
            for (id, options) in &known {
                let Some(present) = current.get(id) else {
                    if options.len() == 1 {
                        continue;
                    }
                    return Err(Error::Invalid("source membership policy mismatch"));
                };
                let permitted: BTreeSet<_> = options.iter().map(String::as_str).collect();
                let received: BTreeSet<_> = present.iter().map(String::as_str).collect();
                if received.iter().any(|id| !permitted.contains(id))
                    || options.iter().any(|option| {
                        !(received.contains(option.as_str())
                            || (options.len() > 1
                                && role == "state"
                                && scope == STATE
                                && EXCEPTIONS.contains(&(*id, option.as_str()))))
                    })
                {
                    return Err(Error::Invalid("source membership policy mismatch"));
                }
            }
        }
        Ok(())
    }
}
fn catalog() -> Result<&'static Membership> {
    static VALUE: OnceLock<Option<Membership>> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            if hex::encode(Sha256::digest(CATALOG)) != CATALOG_SHA256 {
                return None;
            }
            let value: Membership = serde_json::from_slice(CATALOG).ok()?;
            // Canonical encoding and fixed reviewed shape prevent catalog drift.
            let mut sorted = value.clone();
            sorted.sort_unstable();
            if value != sorted || value.len() != 160 || serde_json::to_vec(&value).ok()? != CATALOG
            {
                return None;
            }
            Some(value)
        })
        .as_ref()
        .ok_or(Error::Invalid("invalid bundled source membership catalog"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_catalog_provenance_and_classification_are_pinned() {
        assert_eq!(
            hex::encode(Sha256::digest(CATALOG)),
            "e3a380f76800cae26a72593cc33db5c7f354415c83ab1bff0ea563a27fdefb4b"
        );
        let catalog = catalog().unwrap();
        assert_eq!(catalog.len(), 160);
        assert_eq!(
            catalog
                .iter()
                .flat_map(|s| &s.2)
                .filter(|i| i.1.len() == 1)
                .count(),
            884
        );
        assert_eq!(
            catalog
                .iter()
                .flat_map(|s| &s.2)
                .filter(|i| i.1.len() > 1)
                .count(),
            3294
        );
        assert_eq!(
            catalog
                .iter()
                .filter(|s| s.0 == "state" && s.1 == STATE)
                .count(),
            1
        );
    }
    #[test]
    fn membership_requires_multi_option_items_and_exact_optional_item_options() {
        let policy = TopologyPolicy::Georgia20261103V1;
        let baseline = catalog().unwrap();
        policy.admit(baseline).unwrap();
        let mut omitted = baseline.clone();
        for (role, scope, items) in &mut omitted {
            items.retain(|(_, options)| options.len() != 1);
            if role == "state" && scope == STATE {
                for (id, options) in items {
                    options.retain(|option| !EXCEPTIONS.contains(&(id.as_str(), option.as_str())));
                }
            }
        }
        policy.admit(&omitted).unwrap();
        policy.admit(baseline).unwrap();
        let mut cases = vec![];
        let mut x = baseline.clone();
        let i = x[0].2.iter().position(|i| i.1.len() > 1).unwrap();
        x[0].2.remove(i);
        cases.push(x);
        let mut x = baseline.clone();
        let i = x[0].2.iter().position(|i| i.1.len() > 1).unwrap();
        x[0].2[i].1.pop();
        cases.push(x);
        let mut x = baseline.clone();
        let i = x[0].2.iter().position(|i| i.1.len() == 1).unwrap();
        x[0].2[i].1.clear();
        cases.push(x);
        let mut x = baseline.clone();
        x[0].2[0].0 = "unknown".into();
        cases.push(x);
        let mut x = baseline.clone();
        x[0].2[0].1[0] = "unknown".into();
        cases.push(x);
        let mut x = baseline.clone();
        x.remove(0);
        cases.push(x);
        let mut x = baseline.clone();
        x[0].0 = "state".into();
        cases.push(x);
        let mut x = baseline.clone();
        x[0].1 = "unknown".into();
        cases.push(x);
        for value in cases {
            assert!(policy.admit(&value).is_err());
        }
    }
    #[test]
    fn option_exceptions_are_scoped_and_never_make_required_contests_optional() {
        let policy = TopologyPolicy::Georgia20261103V1;
        let baseline = catalog().unwrap();
        for (item, option) in EXCEPTIONS {
            let mut value = baseline.clone();
            let state = value
                .iter_mut()
                .find(|s| s.0 == "state" && s.1 == STATE)
                .unwrap();
            let contest = state.2.iter_mut().find(|i| i.0 == item).unwrap();
            assert!(contest.1.len() > 1);
            contest.1.retain(|id| id != option);
            policy.admit(&value).unwrap();
            state_remove(&mut value, item);
            assert!(policy.admit(&value).is_err());
        }
        let mut value = baseline.clone();
        let state = value.iter_mut().find(|s| s.0 == "state").unwrap();
        let contest = state.2.iter_mut().find(|i| i.0 == "SHD39").unwrap();
        let required = contest
            .1
            .iter()
            .find(|o| o.as_str() != "YashicaMarshall-86-")
            .unwrap()
            .clone();
        contest.1.retain(|o| o != &required);
        assert!(policy.admit(&value).is_err());
        let mut value = baseline.clone();
        value.retain(|s| s.0 != "state");
        assert!(policy.admit(&value).is_err());
    }
    fn state_remove(value: &mut Membership, item: &str) {
        value
            .iter_mut()
            .find(|s| s.0 == "state")
            .unwrap()
            .2
            .retain(|i| i.0 != item);
    }
}
