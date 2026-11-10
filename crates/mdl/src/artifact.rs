//! Artifacts: a validated bundle written as content-addressed documents.
//!
//! A bundle becomes one catalog artifact, one artifact per family and one
//! per set. Each is written in normal form as canonical JSON (RFC 8785),
//! and its identity is the SHA-256 of that text. A set artifact names the
//! digests of the catalog and family it was validated against; its own
//! digest is the `def_version` consumers stamp on what they decode.
//!
//! Every artifact records the lowest IR version that can read it, so moving
//! the compiler to a newer minor version doesn't change the identity of
//! artifacts that don't use anything new. Packets, commands and composites have no
//! meaningful order, so artifacts list them by name: reordering a bundle
//! never changes an identity.
//!
//! [`rebuild`] keeps a set's artifact, and so its `def_version`, when the
//! set and its family are unchanged and the new catalog is compatible with
//! the previous one; [`Artifacts::from_loaded`] gives it the previous build
//! from storage. [`assemble`] is the way back for consumers: artifacts read
//! from storage become a [`Validated`] bundle only after their pinned
//! digests match and validation passes again.
//!
//! Artifacts never change, so storage keeps every catalog a live set pins:
//! a kept set still pins the catalog it was first built against, and both
//! [`assemble`] and [`Artifacts::from_loaded`] need it. A set decodes
//! against the catalog it pins; what a compatible catalog may change
//! (descriptions, deprecations, limits) is read from the latest catalog.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::canonical::{self, CanonicalError};
use crate::diagnostics::{Code, Diagnostics, Place, Report};
use crate::ir::{ArgumentRange, Artifact, Bundle, Catalog, Command, Digest, IrVersion, Param, Set};
use crate::read::Loaded;
use crate::validate::{Validated, validate};

/// One artifact, written.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Written {
    pub artifact: Artifact,
    /// The SHA-256 of `canonical`.
    pub digest: Digest,
    /// The artifact's canonical JSON: the bytes to store or send.
    pub canonical: String,
}

/// Every artifact of a bundle, families and sets by name.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Artifacts {
    pub catalog: Written,
    pub families: BTreeMap<String, Written>,
    pub sets: BTreeMap<String, Written>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Validation rules out every cause of this; seeing it is a bug.
    #[error("artifact {name} can't be written as JSON: {detail}")]
    Serialize { name: String, detail: String },
    #[error("artifact {name} has no canonical form: {problem}")]
    Canonical {
        name: String,
        problem: CanonicalError,
    },
    /// The new catalog breaks the previous one, so sets validated against
    /// the previous one can't keep their identity.
    #[error("the catalog isn't compatible with the previous one:\n{0}")]
    IncompatibleCatalog(Diagnostics),
}

/// Artifacts read from storage that don't fit together.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AssembleError {
    #[error("expected a {expected} artifact")]
    WrongKind { expected: &'static str },
    #[error("set {set} pins {what} {pinned}, but the {what} given is {given}")]
    DigestMismatch {
        set: String,
        what: &'static str,
        pinned: Digest,
        given: Digest,
    },
    #[error("set {set} pins {what} {pinned}, which wasn't given")]
    Missing {
        set: String,
        what: &'static str,
        pinned: Digest,
    },
    #[error("two {what} artifacts are named {name}")]
    Duplicate { what: &'static str, name: String },
    /// Two sets of one family with one version, which no message could
    /// choose between (artifacts from more than one build).
    #[error(
        "sets {first} and {second} of family {family} both have version {version}; a family's \
         versions are unique"
    )]
    DuplicateVersion {
        family: String,
        version: String,
        first: String,
        second: String,
    },
    /// A set pins an older catalog the given one can't replace, so these
    /// aren't the artifacts of one build history.
    #[error(
        "set {set} pins catalog {pinned}, which the given catalog isn't compatible with:\n{problems}"
    )]
    IncompatibleCatalog {
        set: String,
        pinned: Digest,
        problems: Diagnostics,
    },
    #[error("the assembled artifacts don't validate:\n{0}")]
    Invalid(Diagnostics),
}

impl Artifacts {
    /// A previous build read back from storage, to pass to [`rebuild`].
    /// `catalog` is that build's catalog; `artifacts` are its families and
    /// sets, plus every older catalog one of its sets still pins.
    ///
    /// Each set must pin a family given here and either `catalog` or an
    /// older catalog that `catalog` is compatible with: that's what makes
    /// keeping a set's identity across a rebuild sound, and artifacts from
    /// [`build`] and [`rebuild`] always meet it.
    pub fn from_loaded(
        catalog: Loaded<Artifact>,
        artifacts: impl IntoIterator<Item = Loaded<Artifact>>,
    ) -> Result<Artifacts, AssembleError> {
        let Artifact::Catalog {
            catalog: current, ..
        } = &catalog.value
        else {
            return Err(AssembleError::WrongKind {
                expected: "catalog",
            });
        };
        let mut families = BTreeMap::new();
        let mut sets = BTreeMap::new();
        let mut older = BTreeMap::new();
        for loaded in artifacts {
            let (what, name, into) = match &loaded.value {
                Artifact::Catalog { .. } => {
                    older.insert(loaded.digest.clone(), loaded);
                    continue;
                }
                Artifact::Family { family, .. } => ("family", family.name.clone(), &mut families),
                Artifact::Set { set, .. } => ("set", set.name.clone(), &mut sets),
            };
            if into.contains_key(&name) {
                return Err(AssembleError::Duplicate { what, name });
            }
            into.insert(name, written(loaded));
        }
        for written in sets.values() {
            let Artifact::Set {
                catalog: pinned_catalog,
                family: pinned_family,
                set,
                ..
            } = &written.artifact
            else {
                continue;
            };
            match families.get(&set.family) {
                None => {
                    return Err(AssembleError::Missing {
                        set: set.name.clone(),
                        what: "family",
                        pinned: pinned_family.clone(),
                    });
                }
                Some(family) if &family.digest != pinned_family => {
                    return Err(AssembleError::DigestMismatch {
                        set: set.name.clone(),
                        what: "family",
                        pinned: pinned_family.clone(),
                        given: family.digest.clone(),
                    });
                }
                Some(_) => {}
            }
            if pinned_catalog == &catalog.digest {
                continue;
            }
            let Some(Artifact::Catalog {
                catalog: pinned, ..
            }) = older.get(pinned_catalog).map(|loaded| &loaded.value)
            else {
                return Err(AssembleError::Missing {
                    set: set.name.clone(),
                    what: "catalog",
                    pinned: pinned_catalog.clone(),
                });
            };
            check_compatible(pinned, current).map_err(|problems| {
                AssembleError::IncompatibleCatalog {
                    set: set.name.clone(),
                    pinned: pinned_catalog.clone(),
                    problems,
                }
            })?;
        }
        Ok(Artifacts {
            catalog: written(catalog),
            families,
            sets,
        })
    }
}

fn written(loaded: Loaded<Artifact>) -> Written {
    Written {
        artifact: loaded.value,
        digest: loaded.digest,
        canonical: loaded.canonical,
    }
}

/// Write every artifact of a validated bundle.
pub fn build(validated: &Validated) -> Result<Artifacts, BuildError> {
    let bundle = validated.bundle();
    let catalog_artifact = Artifact::catalog(IrVersion::CURRENT, bundle.catalog.clone());
    let catalog = write("catalog", stamp(catalog_artifact))?;

    let mut families = BTreeMap::new();
    for family in &bundle.families {
        let artifact = Artifact::family(IrVersion::CURRENT, family.clone());
        families.insert(family.name.clone(), write(&family.name, stamp(artifact))?);
    }

    let mut sets = BTreeMap::new();
    for set in &bundle.sets {
        // Validation guarantees the family exists.
        let Some(family) = families.get(&set.family) else {
            continue;
        };
        let artifact = Artifact::set(
            IrVersion::CURRENT,
            catalog.digest.clone(),
            family.digest.clone(),
            in_name_order(set),
        );
        sets.insert(set.name.clone(), write(&set.name, stamp(artifact))?);
    }
    Ok(Artifacts {
        catalog,
        families,
        sets,
    })
}

/// Like [`build`], but a set keeps its previous artifact (and
/// `def_version`) when its content and family are unchanged. That's sound
/// because the new catalog must be compatible with the previous one, and
/// every set of `previous` pins a catalog the previous one is compatible
/// with ([`Artifacts::from_loaded`] checks this for artifacts from
/// storage). Compatibility is transitive, so a kept set's pinned catalog is
/// compatible with the new one, however many rebuilds ago it was built.
pub fn rebuild(validated: &Validated, previous: &Artifacts) -> Result<Artifacts, BuildError> {
    let mut artifacts = build(validated)?;
    if let Artifact::Catalog {
        catalog: before, ..
    } = &previous.catalog.artifact
    {
        check_compatible(before, &validated.bundle().catalog)
            .map_err(BuildError::IncompatibleCatalog)?;
    }
    for (name, written) in artifacts.sets.iter_mut() {
        let Some(kept) = previous.sets.get(name) else {
            continue;
        };
        let unchanged = match (&kept.artifact, &written.artifact) {
            (
                Artifact::Set {
                    family: kept_family,
                    set: kept_set,
                    ..
                },
                Artifact::Set { family, set, .. },
            ) => kept_family == family && kept_set == set,
            _ => false,
        };
        if unchanged {
            *written = kept.clone();
        }
    }
    Ok(artifacts)
}

/// Packets, commands and composites in name order, the one order artifacts
/// use.
fn in_name_order(set: &Set) -> Set {
    let mut set = set.clone();
    set.packets.sort_by(|a, b| a.name.cmp(&b.name));
    set.commands.sort_by(|a, b| a.name.cmp(&b.name));
    for packet in &mut set.packets {
        packet
            .composites
            .sort_by(|a, b| a.canonical.cmp(&b.canonical));
    }
    set
}

/// Artifacts read from storage, put back together for decoding: the set
/// must pin exactly this family and catalog, and the three must validate.
pub fn assemble(
    set: &Loaded<Artifact>,
    family: &Loaded<Artifact>,
    catalog: &Loaded<Artifact>,
) -> Result<Validated, AssembleError> {
    let Artifact::Set {
        catalog: pinned_catalog,
        family: pinned_family,
        set: body,
        ..
    } = &set.value
    else {
        return Err(AssembleError::WrongKind { expected: "set" });
    };
    let Artifact::Family {
        family: family_body,
        ..
    } = &family.value
    else {
        return Err(AssembleError::WrongKind { expected: "family" });
    };
    let Artifact::Catalog {
        catalog: catalog_body,
        ..
    } = &catalog.value
    else {
        return Err(AssembleError::WrongKind {
            expected: "catalog",
        });
    };
    for (what, pinned, given) in [
        ("family", pinned_family, &family.digest),
        ("catalog", pinned_catalog, &catalog.digest),
    ] {
        if pinned != given {
            return Err(AssembleError::DigestMismatch {
                set: body.name.clone(),
                what,
                pinned: pinned.clone(),
                given: given.clone(),
            });
        }
    }
    let mut bundle = Bundle::new(catalog_body.clone());
    bundle.families.push(family_body.clone());
    bundle.sets.push(body.clone());
    validate(bundle).map_err(AssembleError::Invalid)
}

/// The lowest IR version that can read `artifact`. Every feature so far is
/// in 2.0; a feature added in a later minor version raises this only for
/// artifacts that use it.
pub fn required_version(_artifact: &Artifact) -> IrVersion {
    IrVersion::new(2, 0)
}

/// The artifact with its `mdl_version` set to what it needs.
fn stamp(artifact: Artifact) -> Artifact {
    let version = required_version(&artifact);
    match artifact {
        Artifact::Catalog { catalog, .. } => Artifact::catalog(version, catalog),
        Artifact::Family { family, .. } => Artifact::family(version, family),
        Artifact::Set {
            catalog,
            family,
            set,
            ..
        } => Artifact::set(version, catalog, family, set),
    }
}

fn write(name: &str, artifact: Artifact) -> Result<Written, BuildError> {
    let value = serde_json::to_value(&artifact).map_err(|err| BuildError::Serialize {
        name: name.to_string(),
        detail: err.to_string(),
    })?;
    let canonical =
        canonical::to_canonical_string(&value).map_err(|problem| BuildError::Canonical {
            name: name.to_string(),
            problem,
        })?;
    Ok(Written {
        digest: Digest::of_canonical(&canonical),
        artifact,
        canonical,
    })
}

/// Can a catalog replace `old` without breaking anything validated against
/// `old`? Canonical names are a schema: a param may be added, deprecated or
/// redescribed, and its limits may change, but it is never removed, and its
/// type, unit and frame never change. Enum states may only be appended.
/// Commands follow the same rule: never removed; an argument keeps its
/// type, unit and default, its states may only be appended, and its range
/// may only widen; a new argument has a default, so callers written for
/// `old` stay valid (and sets that don't encode it still validate: they
/// send only its default). Set artifacts validated against `old` then stay
/// valid against `new`.
pub fn check_compatible(old: &Catalog, new: &Catalog) -> Result<(), Diagnostics> {
    let mut report = Report::default();
    let catalog = Place::root("").section("catalog");
    for (name, before) in &old.params {
        let place = catalog.map_item("params", name, "param");
        match new.params.get(name) {
            None => report.error(
                Code::RemovedParam,
                &place,
                format!(
                    "{name} is gone; canonical names are never removed (mark it deprecated \
                     instead)"
                ),
            ),
            Some(after) => compare(name, before, after, &place, &mut report),
        }
    }
    for (name, before) in &old.commands {
        let place = catalog.map_item("commands", name, "command");
        match new.commands.get(name) {
            None => report.error(
                Code::RemovedCommand,
                &place,
                format!(
                    "{name} is gone; canonical commands are never removed (mark it deprecated \
                     instead)"
                ),
            ),
            Some(after) => compare_command(name, before, after, &place, &mut report),
        }
    }
    report.finish(())
}

fn compare_command(
    name: &str,
    before: &Command,
    after: &Command,
    place: &Place,
    report: &mut Report,
) {
    let mut changed = |at: Place, what: String| {
        report.error(
            Code::ChangedCommand,
            &at,
            format!("{name}: {what}; add a new command and deprecate this one instead"),
        );
    };
    for argument in &before.arguments {
        let Some((index, now)) = after
            .arguments
            .iter()
            .enumerate()
            .find(|(_, now)| now.name == argument.name)
        else {
            changed(
                place.key("arguments"),
                format!("argument {} is gone", argument.name),
            );
            continue;
        };
        let at = place.item("arguments", index, "argument", &now.name);
        if now.ty != argument.ty {
            changed(
                at.key("type"),
                format!("argument {}'s type changed", now.name),
            );
        }
        if now.unit != argument.unit {
            changed(
                at.key("unit"),
                format!("argument {}'s unit changed", now.name),
            );
        }
        if !now.states.starts_with(&argument.states) {
            changed(
                at.key("states"),
                format!(
                    "argument {}'s states changed (new states may only be added at the end)",
                    now.name
                ),
            );
        }
        if narrows(argument.range, now.range) {
            changed(
                at.key("range"),
                format!(
                    "argument {}'s range is narrower, so a value callers could send is now \
                     refused",
                    now.name
                ),
            );
        }
        if now.default != argument.default {
            changed(
                at.key("default"),
                format!(
                    "argument {}'s default changed, which changes what callers that leave it \
                     out send",
                    now.name
                ),
            );
        }
    }
    for (index, argument) in after.arguments.iter().enumerate() {
        let new = !before.arguments.iter().any(|old| old.name == argument.name);
        if new && argument.default.is_none() {
            changed(
                place.item("arguments", index, "argument", &argument.name),
                format!(
                    "new argument {} has no default, so callers written for the older catalog \
                     would leave it out",
                    argument.name
                ),
            );
        }
    }
}

/// Does `after` refuse a value `before` allowed? An open end allows every
/// value on its side.
fn narrows(before: Option<ArgumentRange>, after: Option<ArgumentRange>) -> bool {
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    let min_raised = match (before.min, after.min) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(old), Some(new)) => new > old,
    };
    let max_lowered = match (before.max, after.max) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(old), Some(new)) => new < old,
    };
    min_raised || max_lowered
}

fn compare(name: &str, before: &Param, after: &Param, place: &Place, report: &mut Report) {
    let mut changed = |key: &str, what: String| {
        report.error(
            Code::ChangedParam,
            &place.key(key),
            format!("{name}'s {what} changed; add a new param and deprecate this one instead"),
        );
    };
    if before.ty != after.ty {
        changed("type", "type".to_string());
    }
    if before.unit != after.unit {
        changed("unit", "unit".to_string());
    }
    if before.frame != after.frame {
        changed("frame", "frame".to_string());
    }
    if !after.states.starts_with(&before.states) {
        changed(
            "states",
            "states (new states may only be added at the end)".to_string(),
        );
    }
}

#[cfg(test)]
mod tests;
