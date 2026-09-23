//! The catalog of English UI text that clients receive in the control schema:
//! each control's name and description, and the labels of its enum values.
//!
//! It is published as `docs/ui-strings.json` so that clients, such as the
//! OpenCPN plugin mayara_pi, can translate this text at build time. The
//! catalog is built from the controls each brand really constructs for every
//! model it knows, and the test below fails when the committed file no longer
//! matches. After changing a control name, description or enum label,
//! regenerate the file and commit it:
//!
//! ```sh
//! UPDATE_UI_STRINGS=1 cargo test ui_strings
//! ```
//!
//! A new brand adds its `controls_for_every_model` to `every_control_set` in
//! the tests below.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddrV4};

use serde::Serialize;
use strum::IntoEnumIterator;

use super::settings::{ControlId, SharedControls};
use super::{RadarInfo, SharedRadars};
use crate::stream::SignalKDelta;
use crate::{Brand, Cli};

const CATALOG_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/ui-strings.json");
const UPDATE_ENV: &str = "UPDATE_UI_STRINGS";

/// A radar for a brand whose model-specific controls need a [`RadarInfo`] to
/// be added to.
pub(crate) fn radar_info<F>(brand: Brand, args: &Cli, controls_fn: F) -> RadarInfo
where
    F: FnOnce(String, tokio::sync::broadcast::Sender<SignalKDelta>) -> SharedControls,
{
    let radars = SharedRadars::new();
    let addr = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 10000);
    let info = RadarInfo::new(
        &radars,
        args,
        brand,
        Some("UISTRINGS1"),
        None,
        None,
        16,
        2048,
        1024,
        addr,
        Ipv4Addr::new(10, 0, 0, 1),
        addr,
        addr,
        addr,
        controls_fn,
        false,
        false,
    );
    // As the locator does, so update_when_model_known finds a name to replace
    info.controls.set_user_name(info.key());
    info
}

#[derive(Serialize)]
struct Catalog {
    controls: Vec<ControlStrings>,
    enums: Vec<EnumStrings>,
}

#[derive(Serialize)]
struct ControlStrings {
    id: String,
    name: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
struct EnumStrings {
    control: String,
    labels: Vec<String>,
}

/// The key clients see for `id` in the controls map of the schema.
fn wire_id(id: ControlId) -> String {
    match serde_json::to_value(id) {
        Ok(serde_json::Value::String(s)) => s,
        other => panic!("{id:?} serialized as {other:?}, not a string"),
    }
}

/// A label that reads the same in every language, such as an STC curve "2".
fn is_numeric(label: &str) -> bool {
    label.parse::<f64>().is_ok() && label.chars().any(|c| c.is_ascii_digit())
}

fn build_catalog(control_sets: &[SharedControls]) -> Catalog {
    let mut controls: Vec<ControlStrings> = ControlId::iter()
        .map(|id| ControlStrings {
            id: wire_id(id),
            name: id.get_name(),
            description: id.get_description(),
        })
        .collect();
    controls.sort_by(|a, b| a.id.cmp(&b.id));

    let mut labels: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for control_set in control_sets {
        for (id, control) in control_set.get_controls() {
            // Range labels are formatted distances such as "1/4 nm", not text
            if id == ControlId::Range {
                continue;
            }
            let Some(descriptions) = &control.item().descriptions else {
                continue;
            };
            let translatable = descriptions.values().filter(|l| !is_numeric(l)).cloned();
            labels.entry(wire_id(id)).or_default().extend(translatable);
        }
    }
    let enums = labels
        .into_iter()
        .filter(|(_, labels)| !labels.is_empty())
        .map(|(control, labels)| EnumStrings {
            control,
            labels: labels.into_iter().collect(),
        })
        .collect();

    Catalog { controls, enums }
}

#[cfg(all(
    feature = "emulator",
    feature = "furuno",
    feature = "garmin",
    feature = "koden",
    feature = "navico",
    feature = "raymarine"
))]
mod tests {
    use clap::Parser;

    use super::*;

    /// Options that switch on every control that is not radar dependent.
    fn every_option() -> Cli {
        Cli::parse_from(["mayara-server", "--stationary", "--targets", "arpa"])
    }

    fn every_control_set(args: &Cli) -> Vec<SharedControls> {
        [
            crate::brand::emulator::controls_for_every_model(args),
            crate::brand::furuno::controls_for_every_model(args),
            crate::brand::garmin::controls_for_every_model(args),
            crate::brand::koden::controls_for_every_model(args),
            crate::brand::navico::controls_for_every_model(args),
            crate::brand::raymarine::controls_for_every_model(args),
        ]
        .concat()
    }

    fn catalog_json() -> String {
        let catalog = build_catalog(&every_control_set(&every_option()));
        let mut json = serde_json::to_string_pretty(&catalog).expect("the catalog serializes");
        json.push('\n');
        json
    }

    fn labels_of<'a>(catalog: &'a Catalog, control: &str) -> &'a [String] {
        catalog
            .enums
            .iter()
            .find(|e| e.control == control)
            .map_or(&[], |e| &e.labels)
    }

    #[test]
    fn ui_strings_json_is_up_to_date() {
        let json = catalog_json();
        if std::env::var_os(UPDATE_ENV).is_some() {
            std::fs::write(CATALOG_PATH, &json).expect("docs/ui-strings.json is writable");
            return;
        }
        let committed = std::fs::read_to_string(CATALOG_PATH)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            committed == json,
            "docs/ui-strings.json is out of date with the control names, descriptions or \
             enum labels. Regenerate it with `{UPDATE_ENV}=1 cargo test ui_strings` and commit it."
        );
    }

    #[test]
    fn ui_strings_lists_every_control_by_its_wire_id() {
        let catalog = build_catalog(&[]);
        assert_eq!(catalog.controls.len(), ControlId::iter().count());
        let sea_state = catalog.controls.iter().find(|c| c.id == "seaState");
        assert_eq!(sea_state.map(|c| c.name), Some("Sea state"));
    }

    #[test]
    fn ui_strings_merge_labels_of_every_brand_and_model() {
        let catalog = build_catalog(&every_control_set(&every_option()));

        // Navico HALO modes and Raymarine Quantum modes share one control
        let mode = labels_of(&catalog, "mode");
        for label in ["Bird+", "Coastal", "Custom", "Harbor"] {
            assert!(mode.iter().any(|l| l == label), "mode lacks {label}");
        }
        // Behind --targets, and brand independent
        assert!(labels_of(&catalog, "targetTrails").contains(&"1 min".to_string()));
    }

    #[test]
    fn ui_strings_leave_out_ranges_and_numbers() {
        let catalog = build_catalog(&every_control_set(&every_option()));

        assert!(labels_of(&catalog, "range").is_empty());
        assert!(labels_of(&catalog, "nearStcCurve").is_empty());
    }
}
