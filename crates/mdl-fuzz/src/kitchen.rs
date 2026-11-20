//! The kitchen sink (`crates/mdl/tests/fixtures/full_bundle.json`) as the
//! targets that decode and encode use it: validated, each family
//! assembled, bindings for every family, and what its catalog and command
//! sets hold. Built once.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use mdl::bindings::Bindings;
use mdl::read::read_bundle;
use mdl::route::FamilySets;
use mdl::validate::{Validated, validate};
use serde_json::Value;

use crate::pipeline::assemble;

const BUNDLE: &[u8] = include_bytes!("../../mdl/tests/fixtures/full_bundle.json");

/// A native id of every family, with a timeline (two of `vendor_j`'s sets,
/// three of `legacy`'s and a retirement), and rows routing refuses: a set
/// for a family with a version locator (`vendor_x` 102), none for one
/// without (`PX-8`), and a set the family hasn't got (`PX-9`).
const BINDINGS: &str = include_str!("../../mdl/tests/fixtures/full_bindings.toml");

pub(crate) struct KitchenSink {
    pub(crate) validated: Validated,
    pub(crate) families: Vec<Family>,
    pub(crate) bindings: Bindings,
    /// The catalog's params.
    pub(crate) params: BTreeSet<String>,
    pub(crate) command_sets: Vec<CommandSet>,
}

/// A family's sets, assembled, with its encoding and the metadata it
/// declares.
pub(crate) struct Family {
    pub(crate) json: bool,
    pub(crate) metadata: Vec<String>,
    pub(crate) sets: FamilySets,
}

/// A set with command packets: its family, by index, and the commands it
/// encodes.
pub(crate) struct CommandSet {
    pub(crate) name: String,
    pub(crate) family: usize,
    pub(crate) commands: Vec<Command>,
}

/// A catalog command and its arguments.
pub(crate) struct Command {
    pub(crate) name: String,
    pub(crate) arguments: Vec<Argument>,
}

/// An argument, with its states if it's an enum.
pub(crate) struct Argument {
    pub(crate) name: String,
    pub(crate) states: Vec<String>,
}

pub(crate) fn kitchen_sink() -> &'static KitchenSink {
    static KITCHEN_SINK: OnceLock<KitchenSink> = OnceLock::new();
    KITCHEN_SINK.get_or_init(build)
}

fn build() -> KitchenSink {
    let bundle = match read_bundle(BUNDLE) {
        Ok(loaded) => loaded.value,
        Err(error) => panic!("the kitchen sink didn't read: {error}"),
    };
    let validated = match validate(bundle) {
        Ok(valid) => valid,
        Err(diagnostics) => panic!("the kitchen sink isn't valid: {diagnostics:?}"),
    };
    let bindings = match Bindings::from_toml(BINDINGS) {
        Ok(bindings) => bindings,
        Err(error) => panic!("the kitchen sink's bindings didn't read: {error}"),
    };
    let json: Value = match serde_json::from_slice(BUNDLE) {
        Ok(json) => json,
        Err(error) => panic!("the kitchen sink isn't JSON: {error}"),
    };
    let mut assembled = assemble(&validated);
    let mut names = Vec::new();
    let mut families = Vec::new();
    for family in list(&json["families"]) {
        let name = text(&family["name"]);
        let Some(at) = assembled.iter().position(|(built, _)| *built == name) else {
            panic!("family {name} wasn't assembled");
        };
        let (_, sets) = assembled.swap_remove(at);
        families.push(Family {
            json: text(&family["encoding"]) == "json",
            metadata: list(&family["metadata"]).iter().map(text).collect(),
            sets,
        });
        names.push(name);
    }
    let catalog = &json["catalog"];
    let params = catalog["params"]
        .as_object()
        .map(|params| params.keys().cloned().collect())
        .unwrap_or_default();
    let command_sets = list(&json["sets"])
        .iter()
        .filter(|set| !list(&set["commands"]).is_empty())
        .map(|set| {
            let family = text(&set["family"]);
            let Some(index) = names.iter().position(|name| *name == family) else {
                panic!("set {} names no family", text(&set["name"]));
            };
            let mut commands: Vec<String> = list(&set["commands"])
                .iter()
                .map(|packet| text(&packet["canonical"]))
                .collect();
            commands.dedup();
            CommandSet {
                name: text(&set["name"]),
                family: index,
                commands: commands
                    .into_iter()
                    .map(|name| Command {
                        arguments: list(&catalog["commands"][&name]["arguments"])
                            .iter()
                            .map(|argument| Argument {
                                name: text(&argument["name"]),
                                states: list(&argument["states"]).iter().map(text).collect(),
                            })
                            .collect(),
                        name,
                    })
                    .collect(),
            }
        })
        .collect();
    KitchenSink {
        validated,
        families,
        bindings,
        params,
        command_sets,
    }
}

fn list(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::kitchen_sink;

    /// Every count the `decode`, `decode_command` and `encode` layouts
    /// choose among by a byte modulo it: families and their metadata
    /// names, command sets, their commands and those commands' arguments.
    #[test]
    fn the_message_layouts_choose_among_what_they_did() {
        let sink = kitchen_sink();
        let families: Vec<usize> = sink
            .families
            .iter()
            .map(|family| family.metadata.len())
            .collect();
        let commands: Vec<Vec<usize>> = sink
            .command_sets
            .iter()
            .map(|set| {
                set.commands
                    .iter()
                    .map(|command| command.arguments.len())
                    .collect()
            })
            .collect();
        assert_eq!(
            (families, commands),
            (
                vec![2, 0, 0, 2, 0],
                vec![vec![2, 2], vec![2], vec![2], vec![4, 1]]
            ),
            "the kitchen sink changed what the message targets' inputs choose \
             among, so every committed input of decode, decode_command and encode \
             means something else: rewrite their seeds, run `just fuzz-reset` for \
             each, and update the counts their layouts' docs give"
        );
    }
}
