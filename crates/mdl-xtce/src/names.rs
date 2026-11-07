//! XTCE names as MDL names.
//!
//! XTCE names may hold almost any character; MDL's are narrower. Entry and
//! packet names keep their XTCE spelling when it's an MDL identifier.
//! Catalog names are lowercase and dotted: a parameter's alias in the MDL
//! namespace when it has one, else its SpaceSystem path and name.

use std::collections::BTreeSet;

/// One part of a catalog name: lowercase ASCII letters and digits, each
/// run of anything else one `_`, none at either end. `None` if nothing is
/// left.
pub(crate) fn catalog_part(name: &str) -> Option<String> {
    let mut part = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            part.push(c.to_ascii_lowercase());
        } else if !part.ends_with('_') {
            part.push('_');
        }
    }
    let trimmed = part.trim_matches('_');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A name as an MDL identifier (letters, digits and `_`, not starting with
/// a digit), and whether it had to change.
pub(crate) fn identifier(name: &str) -> (String, bool) {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) || out.is_empty() {
        out.insert(0, '_');
    }
    let changed = out != name;
    (out, changed)
}

/// MDL names for items that share a name in different SpaceSystems, as
/// `(name, SpaceSystem names from the root down)`: each qualified by as
/// many of its SpaceSystems' names, nearest first, as it takes to tell
/// them apart (`EPS_HK`, `ADCS_HK`). One item keeps its own name, unless
/// `always`: then every name has its SpaceSystems below the root, and the
/// root too where that leaves two alike.
pub(crate) fn qualified(items: &[(&str, Vec<&str>)], always: bool) -> Vec<String> {
    let own = |name: &str| identifier(name).0;
    let with = |skip: usize| -> Vec<String> {
        items
            .iter()
            .map(|(name, path)| {
                let mut parts = path.get(skip..).unwrap_or_default().to_vec();
                parts.push(name);
                own(&parts.join("_"))
            })
            .collect()
    };
    if always {
        let below = with(1);
        let distinct: BTreeSet<&String> = below.iter().collect();
        return if distinct.len() == below.len() {
            below
        } else {
            with(0)
        };
    }
    if items.len() < 2 {
        return items.iter().map(|(name, _)| own(name)).collect();
    }
    let deepest = items.iter().map(|(_, path)| path.len()).max().unwrap_or(0);
    let mut names = items.iter().map(|(name, _)| own(name)).collect::<Vec<_>>();
    for take in 1..=deepest {
        names = items
            .iter()
            .map(|(name, path)| {
                let mut parts = path[path.len().saturating_sub(take)..].to_vec();
                parts.push(name);
                own(&parts.join("_"))
            })
            .collect();
        let distinct: BTreeSet<&String> = names.iter().collect();
        if distinct.len() == names.len() {
            break;
        }
    }
    names
}

/// A family or set name: lowercase letters, digits and `_`, starting with
/// a letter. `None` if the name doesn't give one.
pub(crate) fn token(name: &str) -> Option<String> {
    catalog_part(name).filter(|t| t.starts_with(|c: char| c.is_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parts_are_lowercase_words() {
        assert_eq!(catalog_part("Bus_Voltage").as_deref(), Some("bus_voltage"));
        assert_eq!(
            catalog_part("BusElectronics").as_deref(),
            Some("buselectronics")
        );
        assert_eq!(catalog_part("Bat-Temp #2").as_deref(), Some("bat_temp_2"));
        assert_eq!(catalog_part("__x__").as_deref(), Some("x"));
        assert_eq!(catalog_part("3V3").as_deref(), Some("3v3"));
        assert_eq!(catalog_part("°°"), None);
    }

    #[test]
    fn identifiers_keep_names_that_fit() {
        assert_eq!(
            identifier("Battery1_Voltage"),
            ("Battery1_Voltage".into(), false)
        );
        assert_eq!(identifier("Bat-Temp"), ("Bat_Temp".into(), true));
        assert_eq!(identifier("3V3"), ("_3V3".into(), true));
        assert_eq!(identifier("Température"), ("Temp_rature".into(), true));
    }

    #[test]
    fn shared_names_take_as_much_of_their_paths_as_they_need() {
        let short = |items: &[(&str, Vec<&str>)]| qualified(items, false);
        assert_eq!(short(&[("HK", vec!["Sat", "EPS"])]), ["HK"]);
        assert_eq!(
            short(&[("HK", vec!["Sat", "EPS"]), ("HK", vec!["Sat", "ADCS"])]),
            ["EPS_HK", "ADCS_HK"]
        );
        assert_eq!(
            short(&[("HK", vec!["Sat"]), ("HK", vec!["Sat", "EPS"])]),
            ["Sat_HK", "EPS_HK"]
        );
        assert_eq!(
            short(&[("T", vec!["Sat", "A", "X"]), ("T", vec!["Sat", "B", "X"])]),
            ["A_X_T", "B_X_T"]
        );
        // Names that only become one as identifiers are shared too.
        assert_eq!(
            short(&[("Bus-V", vec!["Sat", "A"]), ("Bus_V", vec!["Sat", "B"])]),
            ["A_Bus_V", "B_Bus_V"]
        );
    }

    #[test]
    fn always_qualified_names_carry_their_path_below_the_root() {
        assert_eq!(qualified(&[("HK", vec!["Sat", "EPS"])], true), ["EPS_HK"]);
        assert_eq!(qualified(&[("HK", vec!["Sat"])], true), ["HK"]);
        // Two roots with an HK each: the roots tell them apart.
        assert_eq!(
            qualified(&[("HK", vec!["A"]), ("HK", vec!["B"])], true),
            ["A_HK", "B_HK"]
        );
    }

    #[test]
    fn tokens_start_with_a_letter() {
        assert_eq!(token("BogusSAT").as_deref(), Some("bogussat"));
        assert_eq!(token("my project").as_deref(), Some("my_project"));
        assert_eq!(token("3U-Sat"), None);
    }
}
