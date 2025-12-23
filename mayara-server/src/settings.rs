use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, RwLock},
};
use thiserror::Error;

use mayara_core::capabilities::ControlDefinition as CoreControlDefinition;

use crate::{
    control_factory,
    radar::{range::Ranges, DopplerMode, Legend, RadarError, Status},
    Session, TargetMode,
};

///
/// Radars have settings. There are some common ones that every radar supports:
/// range, gain, sea clutter and rain clutter. Some others are less common, and
/// are usually expressed in slightly different ways.
/// For instance, a radar may have an interference rejection setting. Some will
/// have two possible values (off or on) whilst others may have multiple levels,
/// like off, low, medium and high.
///
/// To cater for this, we keep the state of these settings in generalized state
/// structures in Rust.
///
/// Per radar we keep a single Controls structure in memory that is
/// accessed from all threads that are working for that radar and any user
/// clients.
///
/// If you've got a reference to the controls object for a radar, you can
/// subscribe to any changes made to it.
///

#[derive(Clone, Debug, Serialize)]
pub struct Controls {
    #[serde(skip)]
    session: Session,

    /// Controls stored by string ID (SignalK camelCase format)
    #[serde(flatten)]
    controls: HashMap<String, Control>,

    #[serde(skip)]
    all_clients_tx: tokio::sync::broadcast::Sender<ControlValue>,
    #[serde(skip)]
    control_update_tx: tokio::sync::broadcast::Sender<ControlUpdate>,
    #[serde(skip)]
    data_update_tx: tokio::sync::broadcast::Sender<DataUpdate>,
}

impl Controls {
    pub(self) fn insert(&mut self, id: &str, value: Control) {
        let v = Control {
            item: ControlDefinition {
                is_read_only: self.session.read().unwrap().args.replay || value.item.is_read_only,
                ..value.item
            },
            ..value
        };
        self.controls.insert(id.to_string(), v);
    }

    pub(self) fn new_base(session: Session, controls: HashMap<String, Control>) -> Self {
        let mut string_controls = controls;

        // Add _mandatory_ controls
        if !string_controls.contains_key("modelName") {
            string_controls.insert(
                "modelName".to_string(),
                Control::new_string("modelName")
                    .read_only(true)
                    .set_destination(ControlDestination::Internal),
            );
        }

        if session.read().unwrap().args.replay {
            string_controls.iter_mut().for_each(|(_k, v)| {
                v.item.is_read_only = true;
            });
        }

        // Add controls that are not radar dependent
        string_controls.insert(
            "userName".to_string(),
            Control::new_string("userName")
                .read_only(false)
                .set_destination(ControlDestination::Internal),
        );

        if session.read().unwrap().args.targets != TargetMode::None {
            string_controls.insert(
                "targetTrails".to_string(),
                Control::new_map(
                    "targetTrails",
                    HashMap::from([
                        (0, "Off".to_string()),
                        (1, "15s".to_string()),
                        (2, "30s".to_string()),
                        (3, "1 min".to_string()),
                        (4, "3 min".to_string()),
                        (5, "5 min".to_string()),
                        (6, "10 min".to_string()),
                    ]),
                )
                .set_destination(ControlDestination::Data),
            );

            string_controls.insert(
                "trailsMotion".to_string(),
                Control::new_map(
                    "trailsMotion",
                    HashMap::from([(0, "Relative".to_string()), (1, "True".to_string())]),
                )
                .set_destination(ControlDestination::Data),
            );

            string_controls.insert(
                "clearTrails".to_string(),
                Control::new_button("clearTrails").set_destination(ControlDestination::Data),
            );

            if session.read().unwrap().args.targets == TargetMode::Arpa {
                string_controls.insert(
                    "clearTargets".to_string(),
                    Control::new_button("clearTargets").set_destination(ControlDestination::Data),
                );
            }
        }

        let (all_clients_tx, _) = tokio::sync::broadcast::channel(32);
        let (control_update_tx, _) = tokio::sync::broadcast::channel(32);
        let (data_update_tx, _) = tokio::sync::broadcast::channel(64);

        Controls {
            session: session.clone(),
            controls: string_controls,
            all_clients_tx,
            control_update_tx,
            data_update_tx,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SharedControls {
    #[serde(flatten, with = "arc_rwlock_serde")]
    controls: Arc<RwLock<Controls>>,
}

mod arc_rwlock_serde {
    use serde::ser::Serializer;
    use serde::Serialize;
    use std::sync::{Arc, RwLock};

    pub fn serialize<S, T>(val: &Arc<RwLock<T>>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        T::serialize(&*val.read().unwrap(), s)
    }
}

impl SharedControls {
    // Create a new set of controls, for a radar.
    // There is only one set that is shared amongst the various threads and
    // structs, hence the word Shared.
    pub fn new(session: Session, mut controls: HashMap<String, Control>) -> Self {
        // All radars must have the same Status control - use core definition
        let mut control = control_factory::power_control().send_always();
        control.set_valid_values([1, 2].to_vec()); // Only allow setting to Standby (index 1) and Transmit (index 2)
        controls.insert("power".to_string(), control);

        SharedControls {
            controls: Arc::new(RwLock::new(Controls::new_base(session, controls))),
        }
    }

    pub(crate) fn get_data_update_tx(&self) -> tokio::sync::broadcast::Sender<DataUpdate> {
        let locked = self.controls.read().unwrap();

        locked.data_update_tx.clone()
    }

    fn get_command_tx(&self) -> tokio::sync::broadcast::Sender<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.clone()
    }

    pub(crate) fn all_clients_rx(&self) -> tokio::sync::broadcast::Receiver<ControlValue> {
        let locked = self.controls.read().unwrap();

        locked.all_clients_tx.subscribe()
    }

    // process_client_request()
    //
    // In theory this could be from anywhere that somebody holds a SharedControls reference,
    // but in practice only called from the websocket request handler in web.rs.
    // The end user has sent a control update and we need to process this.
    //
    // Some controls are handled internally, some in the data handler for a radar and the
    // rest are settings that need to be sent to the radar.
    //
    pub async fn process_client_request(
        &self,
        control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let control = self.get(&control_value.id);

        if let Err(e) = match control {
            Some(c) => {
                log::debug!(
                    "Client request to update {:?} to {:?}",
                    ControlValue::from(&c, None),
                    control_value
                );
                match c.item().destination {
                    ControlDestination::Internal => self
                        // set_string will also set numeric values
                        .set_string("userName", control_value.value.clone())
                        .map(|_| ())
                        .map_err(|e| RadarError::ControlError(e)),
                    ControlDestination::Data => {
                        self.send_to_data_handler(&reply_tx, control_value.clone())
                    }
                    ControlDestination::Command => {
                        self.send_to_command_handler(control_value.clone(), reply_tx.clone())
                    }
                }
            }
            None => Err(RadarError::CannotSetControlType(control_value.id.clone())),
        } {
            self.send_error_to_client(reply_tx, &control_value, &e)
                .await
        } else {
            Ok(())
        }
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.subscribe()
    }

    pub fn data_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<DataUpdate> {
        let locked = self.controls.read().unwrap();

        locked.data_update_tx.subscribe()
    }

    pub async fn send_all_controls(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let controls: Vec<Control> = {
            let locked = self.controls.read().unwrap();

            locked.controls.clone().into_values().collect()
        };

        for c in controls {
            self.send_reply_to_client(reply_tx.clone(), &c, None)
                .await?;
        }
        Ok(())
    }

    fn send_to_data_handler(
        &self,
        reply_tx: &tokio::sync::mpsc::Sender<ControlValue>,
        cv: ControlValue,
    ) -> Result<(), RadarError> {
        self.get_data_update_tx()
            .send(DataUpdate::ControlValue(reply_tx.clone(), cv))
            .map(|_| ())
            .map_err(|_| RadarError::Shutdown)
    }

    fn send_to_command_handler(
        &self,
        control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let control_update = ControlUpdate {
            control_value,
            reply_tx,
        };
        self.get_command_tx()
            .send(control_update)
            .map(|_| ())
            .map_err(|_| RadarError::Shutdown)
    }

    fn send_to_all_clients(&self, control: &Control) {
        let control_value = crate::settings::ControlValue {
            id: control.item().id.clone(),
            value: control.value(),
            auto: control.auto,
            enabled: control.enabled,
            error: None,
        };

        let locked = self.controls.read().unwrap();
        match locked.all_clients_tx.send(control_value) {
            Err(_e) => {}
            Ok(cnt) => {
                log::trace!(
                    "Sent control value {} to {} JSON clients",
                    control.item().id,
                    cnt
                );
            }
        }
    }

    pub async fn send_reply_to_client(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
        control: &Control,
        error: Option<String>,
    ) -> Result<(), RadarError> {
        let control_value = ControlValue::from(control, error);

        log::debug!(
            "Sending reply {:?} to requesting JSON client",
            &control_value,
        );

        reply_tx
            .send(control_value)
            .await
            .map_err(|_| RadarError::Shutdown)
    }

    pub async fn send_error_to_client(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
        cv: &ControlValue,
        e: &RadarError,
    ) -> Result<(), RadarError> {
        if let Some(control) = self.get(&cv.id) {
            self.send_reply_to_client(reply_tx, &control, Some(e.to_string()))
                .await?;
            log::warn!("User tried to set invalid {}: {}", cv.id, e);
            Ok(())
        } else {
            Err(RadarError::CannotSetControlType(cv.id.clone()))
        }
    }

    // ******* GET & SET METHODS

    pub fn insert(&self, id: &str, value: Control) {
        let mut locked = self.controls.write().unwrap();

        locked.insert(id, value);
    }

    pub fn get(&self, id: &str) -> Option<Control> {
        let locked = self.controls.read().unwrap();
        locked.controls.get(id).cloned()
    }

    /// Get all control IDs and their Control values
    pub fn get_all(&self) -> Vec<(String, Control)> {
        let locked = self.controls.read().unwrap();
        locked
            .controls
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Look up a control by its API name (case-insensitive, camelCase)
    pub fn get_by_name(&self, name: &str) -> Option<Control> {
        let locked = self.controls.read().unwrap();
        let name_lower = name.to_lowercase();

        // Keys are already SignalK camelCase IDs, just do case-insensitive lookup
        for (key, control) in locked.controls.iter() {
            if key.to_lowercase() == name_lower {
                return Some(control.clone());
            }
        }
        None
    }

    pub fn set_refresh(&self, id: &str) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(id) {
            control.needs_refresh = true;
        }
    }

    pub fn set_value_auto_enabled<T>(
        &self,
        id: &str,
        value: T,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError>
    where
        f32: From<T>,
    {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(id) {
                Ok(control
                    .set(value.into(), None, auto, enabled)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(id.to_string()))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_wire_range(&self, id: &str, min: f32, max: f32) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(id) {
                Ok(control.set_wire_range(min, max)?.map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(id.to_string()))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    //
    // Set a control from a wire value, so apply all transformations
    // to convert it to a user visible value
    //
    pub fn set(
        &self,
        id: &str,
        value: f32,
        auto: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(id) {
                Ok(control
                    .set(value, None, auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(id.to_string()))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_auto_state(&self, id: &str, auto: bool) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(id) {
            control.set_auto(auto);
        } else {
            return Err(ControlError::NotSupported(id.to_string()));
        };
        Ok(())
    }

    pub fn set_value_auto(
        &self,
        id: &str,
        auto: bool,
        value: f32,
    ) -> Result<Option<()>, ControlError> {
        self.set(id, value, Some(auto))
    }

    pub fn set_value_with_many_auto(
        &self,
        id: &str,
        value: f32,
        auto_value: f32,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(id) {
                let auto = control.auto;
                Ok(control
                    .set(value, Some(auto_value), auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(id.to_string()))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(&self, id: &str, value: String) -> Result<Option<String>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(id) {
                if control.item().data_type == ControlDataType::String {
                    Ok(control.set_string(value).map(|_| control.clone()))
                } else {
                    let i = value
                        .parse::<i32>()
                        .map_err(|_| ControlError::Invalid(id.to_string(), value))?;
                    control
                        .set(i as f32, None, None, None)
                        .map(|_| Some(control.clone()))
                }
            } else {
                Err(ControlError::NotSupported(id.to_string()))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(control.description.clone())
        } else {
            Ok(None)
        }
    }

    pub fn set_user_name(&self, name: String) {
        let mut locked = self.controls.write().unwrap();
        let control = locked.controls.get_mut("userName").unwrap();
        control.set_string(name);
    }

    pub fn user_name(&self) -> Option<String> {
        self.get("userName").and_then(|c| c.description)
    }

    pub fn set_model_name(&self, name: String) {
        let mut locked = self.controls.write().unwrap();
        let control = locked.controls.get_mut("modelName").unwrap();
        control.set_string(name.clone());
    }

    pub fn model_name(&self) -> Option<String> {
        self.get("modelName").and_then(|c| c.description)
    }

    pub fn set_valid_values(&self, id: &str, valid_values: Vec<i32>) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        locked
            .controls
            .get_mut(id)
            .ok_or(ControlError::NotSupported(id.to_string()))
            .map(|c| {
                c.set_valid_values(valid_values);
                ()
            })
    }

    pub fn set_valid_ranges(&self, id: &str, ranges: &Ranges) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        locked
            .controls
            .get_mut(id)
            .ok_or(ControlError::NotSupported(id.to_string()))
            .map(|c| {
                c.set_valid_ranges(ranges);
                ()
            })
    }

    pub(crate) fn get_status(&self) -> Option<Status> {
        let locked = self.controls.read().unwrap();
        if let Some(control) = locked.controls.get("power") {
            return Status::from_str(&control.value()).ok();
        }

        None
    }
}

#[derive(Clone, Debug)]
pub struct ControlUpdate {
    pub reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    pub control_value: ControlValue,
}

// Messages sent to Data receiver
#[derive(Clone, Debug)]
pub enum DataUpdate {
    Doppler(DopplerMode),
    Legend(Legend),
    Ranges(Ranges),
    ControlValue(tokio::sync::mpsc::Sender<ControlValue>, ControlValue),
}

// This is what we send back and forth to clients
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControlValue {
    /// Control ID in SignalK camelCase format (e.g., "gain", "dopplerMode")
    pub id: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlValue {
    pub fn new(id: &str, value: String) -> Self {
        ControlValue {
            id: id.to_string(),
            value,
            auto: None,
            enabled: None,
            error: None,
        }
    }

    pub fn from(control: &Control, error: Option<String>) -> Self {
        ControlValue {
            id: control.item().id.clone(),
            value: control.value(),
            auto: control.auto,
            enabled: control.enabled,
            error,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    #[serde(flatten)]
    item: ControlDefinition,
    /// Reference to core control definition (source of truth for enum values, etc.)
    #[serde(skip)]
    core_def: Option<Arc<CoreControlDefinition>>,
    #[serde(skip)]
    pub value: Option<f32>,
    #[serde(skip)]
    pub auto_value: Option<f32>,
    #[serde(skip)]
    pub description: Option<String>,
    #[serde(skip)]
    pub auto: Option<bool>,
    #[serde(skip)]
    pub enabled: Option<bool>,
    #[serde(skip)]
    pub needs_refresh: bool, // True when it has been changed and client needs to know value (again)
}

impl Control {
    fn new(item: ControlDefinition) -> Self {
        let value = item.default_value;
        Control {
            item,
            core_def: None,
            value,
            auto_value: None,
            auto: None,
            enabled: None,
            description: None,
            needs_refresh: false,
        }
    }

    /// Create a new Control with a reference to the core definition
    pub fn with_core_def(mut self, core_def: Arc<CoreControlDefinition>) -> Self {
        self.core_def = Some(core_def);
        self
    }

    /// Get the core control definition if available
    pub fn core_definition(&self) -> Option<&CoreControlDefinition> {
        self.core_def.as_ref().map(|arc| arc.as_ref())
    }

    /// Look up the wire value (index) for an enum value by its string value or label
    /// Returns None if no match found or if not an enum control
    pub fn enum_value_to_index(&self, value_str: &str) -> Option<usize> {
        // First try to get from core definition
        if let Some(core_def) = &self.core_def {
            if let Some(values) = &core_def.values {
                // Try exact match on value first
                for (idx, ev) in values.iter().enumerate() {
                    if let Some(s) = ev.value.as_str() {
                        if s.eq_ignore_ascii_case(value_str) {
                            return Some(idx);
                        }
                    }
                }
                // Then try match on label
                for (idx, ev) in values.iter().enumerate() {
                    if ev.label.eq_ignore_ascii_case(value_str) {
                        return Some(idx);
                    }
                }
            }
        }
        // Fallback to local descriptions
        if let Some(descriptions) = &self.item.descriptions {
            for (idx, label) in descriptions.iter() {
                if label.eq_ignore_ascii_case(value_str) {
                    return Some(*idx as usize);
                }
            }
        }
        None
    }

    /// Look up the string value for an enum by its index
    /// Returns the core definition's value if available, otherwise the label
    pub fn index_to_enum_value(&self, index: usize) -> Option<String> {
        // First try to get from core definition
        if let Some(core_def) = &self.core_def {
            if let Some(values) = &core_def.values {
                if let Some(ev) = values.get(index) {
                    // Return the value field (e.g., "transmit") not the label
                    if let Some(s) = ev.value.as_str() {
                        return Some(s.to_string());
                    }
                    // For numeric values, return the label
                    return Some(ev.label.clone());
                }
            }
        }
        // Fallback to local descriptions
        if let Some(descriptions) = &self.item.descriptions {
            return descriptions.get(&(index as i32)).cloned();
        }
        None
    }

    pub fn read_only(mut self, is_read_only: bool) -> Self {
        self.item.is_read_only = is_read_only;

        self
    }

    pub fn set_destination(mut self, destination: ControlDestination) -> Self {
        self.item.destination = destination;

        self
    }

    pub fn wire_scale_factor(mut self, wire_scale_factor: f32, with_step: bool) -> Self {
        self.item.wire_scale_factor = Some(wire_scale_factor);
        if with_step {
            self.item.step_value =
                Some(self.item.max_value.unwrap_or(1.) / self.item.wire_scale_factor.unwrap_or(1.));
        }

        self
    }

    pub fn wire_offset(mut self, wire_offset: f32) -> Self {
        self.item.wire_offset = Some(wire_offset);

        self
    }

    pub fn unit<S: AsRef<str>>(mut self, unit: S) -> Control {
        self.item.unit = Some(unit.as_ref().to_string());

        self
    }

    /// Override the maximum value (used for enum controls with non-sequential values)
    pub fn max_value(mut self, max: f32) -> Control {
        self.item.max_value = Some(max);
        // Also update wire_scale_factor to match, as it's used for validation
        self.item.wire_scale_factor = Some(max);
        self
    }

    pub fn send_always(mut self) -> Control {
        self.item.is_send_always = true;

        self
    }

    pub fn has_enabled(mut self) -> Self {
        self.item.has_enabled = true;

        self
    }

    pub fn new_numeric(id: &str, min_value: f32, max_value: f32) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        let control = Self::new(ControlDefinition {
            id: id.to_string(),
            name: id.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: min_value,
            min_value,
            max_value,
            step_value: None,
            wire_scale_factor: max_value,
            wire_offset: None,
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: false,
            data_type: ControlDataType::Number,
            is_send_always: false,
            destination: ControlDestination::Command,
        });
        control
    }

    pub fn new_auto(id: &str, min_value: f32, max_value: f32, automatic: AutomaticValue) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        Self::new(ControlDefinition {
            id: id.to_string(),
            name: id.to_string(),
            automatic: Some(automatic),
            has_enabled: false,
            default_value: min_value,
            min_value,
            max_value,
            step_value: None,
            wire_scale_factor: max_value,
            wire_offset: None,
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: false,
            data_type: ControlDataType::Number,
            is_send_always: false,
            destination: ControlDestination::Command,
        })
    }

    pub fn new_list(id: &str, descriptions: &[&str]) -> Self {
        let description_count = ((descriptions.len() as i32) - 1) as f32;
        Self::new(ControlDefinition {
            id: id.to_string(),
            name: id.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: Some(0.),
            min_value: Some(0.),
            max_value: Some(description_count),
            step_value: None,
            wire_scale_factor: Some(description_count),
            wire_offset: None,
            unit: None,
            descriptions: Some(
                descriptions
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| (i as i32, n.to_string()))
                    .collect(),
            ),
            valid_values: None,
            is_read_only: false,
            data_type: ControlDataType::Number,
            is_send_always: false,
            destination: ControlDestination::Command,
        })
    }

    pub fn new_map(id: &str, descriptions: HashMap<i32, String>) -> Self {
        Self::new(ControlDefinition {
            id: id.to_string(),
            name: id.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: Some(0.),
            min_value: Some(0.),
            max_value: Some(((descriptions.len() as i32) - 1) as f32),
            step_value: None,
            wire_scale_factor: Some(((descriptions.len() as i32) - 1) as f32),
            wire_offset: None,
            unit: None,
            descriptions: Some(descriptions),
            valid_values: None,
            is_read_only: false,
            data_type: ControlDataType::Number,
            is_send_always: false,
            destination: ControlDestination::Command,
        })
    }

    pub fn new_string(id: &str) -> Self {
        let control = Self::new(ControlDefinition {
            id: id.to_string(),
            name: id.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: None,
            min_value: None,
            max_value: None,
            step_value: None,
            wire_scale_factor: None,
            wire_offset: None,
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: true,
            data_type: ControlDataType::String,
            is_send_always: false,
            destination: ControlDestination::Command,
        });
        control
    }

    pub fn new_button(id: &str) -> Self {
        let control = Self::new(ControlDefinition {
            id: id.to_string(),
            name: id.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: None,
            min_value: None,
            max_value: None,
            step_value: None,
            wire_scale_factor: None,
            wire_offset: None,
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: false,
            data_type: ControlDataType::Button,
            is_send_always: false,
            destination: ControlDestination::Command,
        });
        control
    }

    /// Read-only access to the definition of the control
    pub fn item(&self) -> &ControlDefinition {
        &self.item
    }

    /// Get the control ID (SignalK camelCase format)
    pub fn id(&self) -> &str {
        &self.item.id
    }

    pub fn set_valid_values(&mut self, values: Vec<i32>) {
        self.item.valid_values = Some(values);
    }

    pub fn set_valid_ranges(&mut self, ranges: &Ranges) {
        let mut values = Vec::new();
        let mut descriptions = HashMap::new();
        for range in ranges.all.iter() {
            values.push(range.distance());
            descriptions.insert(range.distance() as i32, format!("{}", range));
        }

        self.item.valid_values = Some(values);
        self.item.descriptions = Some(descriptions);
    }

    // pub fn auto(&self) -> Option<bool> {
    //     self.auto
    // }

    pub fn value(&self) -> String {
        if self.item.data_type == ControlDataType::String {
            return self.description.clone().unwrap_or_else(|| "".to_string());
        }

        if self.auto.unwrap_or(false) && self.auto_value.is_some() {
            return self.auto_value.unwrap().to_string();
        }

        self.value
            .unwrap_or(self.item.default_value.unwrap_or(0.))
            .to_string()
    }

    pub fn set_auto(&mut self, auto: bool) {
        self.needs_refresh = self.auto != Some(auto);
        log::trace!(
            "Setting {} auto {} changed: {}",
            self.item.id,
            auto,
            self.needs_refresh
        );
        self.auto = Some(auto);
    }

    ///
    /// Set a control from a wire value, so apply all transformations
    /// to convert it to a user visible value
    ///
    /// Set the control to a (maybe new) value + auto state
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub fn set(
        &mut self,
        mut value: f32,
        mut auto_value: Option<f32>,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        // SCALE MAPPING
        log::trace!(
            "{}: set(value={},auto_value={:?},auto={:?},enabled={:?}) with item {:?}",
            self.item.name,
            value,
            auto_value,
            auto,
            enabled,
            self.item
        );
        if let Some(wire_offset) = self.item.wire_offset {
            if wire_offset > 0.0 {
                value -= wire_offset;
            }
        }
        if let (Some(wire_scale_factor), Some(max_value)) =
            (self.item.wire_scale_factor, self.item.max_value)
        {
            // One of the reasons we use f32 is because Navico wire format for some things is
            // tenths of degrees. To make things uniform we map these to a float with .1 precision.
            if wire_scale_factor != max_value {
                log::trace!("{} map value {}", self.item.id, value);
                value = value * max_value / wire_scale_factor;

                // TODO! Not sure about the following line
                auto_value = auto_value.map(|v| v * max_value / wire_scale_factor);
                log::trace!("{} map value to scaled {}", self.item.id, value);
            }
        }

        // RANGE MAPPING
        if let (Some(min_value), Some(max_value)) = (self.item.min_value, self.item.max_value) {
            if self.item.wire_offset.unwrap_or(0.) == -1.
                && value > max_value
                && value <= 2. * max_value
            {
                // debug!("{} value {} -> ", self.item.id, value);
                value -= 2. * max_value;
                // debug!("{} ..... {}", self.item.id, value);
            }

            if value < min_value {
                return Err(ControlError::TooLow(self.item.id.clone(), value, min_value));
            }
            if value > max_value {
                return Err(ControlError::TooHigh(
                    self.item.id.clone(),
                    value,
                    max_value,
                ));
            }
        }

        let step = self.item.step_value.unwrap_or(1.0);
        match step {
            0.1 => {
                value = (value * 10.) as i32 as f32 / 10.;
                auto_value = auto_value.map(|value| (value * 10.) as i32 as f32 / 10.);
            }
            1.0 => {
                value = value as i32 as f32;
                auto_value = auto_value.map(|value| value as i32 as f32);
            }
            _ => {
                value = (value / step).round() * step;
                auto_value = auto_value.map(|value| (value / step).round() * step);
            }
        }
        log::trace!("{} map value to rounded {}", self.item.id, value);

        if auto.is_some() && self.item.automatic.is_none() {
            Err(ControlError::NoAuto(self.item.id.clone()))
        } else if self.value != Some(value)
            || self.auto_value != auto_value
            || self.auto != auto
            || self.enabled != enabled
        {
            self.value = Some(value);
            self.auto_value = auto_value;
            self.auto = auto;
            self.enabled = enabled;
            self.needs_refresh = false;

            Ok(Some(()))
        } else if self.needs_refresh || self.item.is_send_always {
            self.needs_refresh = false;
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(&mut self, value: String) -> Option<()> {
        let value = Some(value);
        if &self.description != &value {
            self.description = value;
            self.needs_refresh = false;
            log::trace!("Set {} to {:?}", self.item.id, self.description);
            Some(())
        } else if self.needs_refresh {
            self.needs_refresh = false;
            Some(())
        } else {
            None
        }
    }

    /// Set the control's wire offset and scale
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub fn set_wire_range(&mut self, min: f32, max: f32) -> Result<Option<()>, ControlError> {
        let max = Some(max - min);
        let min = if min != 0.0 { Some(min) } else { None };

        if min != self.item.wire_offset || max != self.item.wire_scale_factor {
            log::debug!(
                "{}: new wire offset {:?} and scale {:?}",
                self.item.name,
                min,
                max,
            );
            self.item.wire_offset = min;
            self.item.wire_scale_factor = max;
        }
        Ok(None)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticValue {
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_auto: bool,
    //#[serde(skip)]
    //pub(crate) auto_values: i32,
    //#[serde(skip)]
    //pub(crate) auto_descriptions: Option<Vec<String>>,
    pub(crate) has_auto_adjustable: bool,
    pub(crate) auto_adjust_min_value: f32,
    pub(crate) auto_adjust_max_value: f32,
}

pub(crate) const HAS_AUTO_NOT_ADJUSTABLE: AutomaticValue = AutomaticValue {
    has_auto: true,
    has_auto_adjustable: false,
    auto_adjust_min_value: 0.,
    auto_adjust_max_value: 0.,
};

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ControlDataType {
    Number,
    String,
    Button,
}

#[derive(Clone, Debug)]
pub enum ControlDestination {
    Internal,
    Data,
    Command,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDefinition {
    /// Control ID in SignalK camelCase format (e.g., "gain", "dopplerMode")
    #[serde(skip)]
    pub(crate) id: String,
    name: String,
    pub(crate) data_type: ControlDataType,
    //#[serde(skip)]
    //has_off: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    automatic: Option<AutomaticValue>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_enabled: bool,
    #[serde(skip)]
    default_value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_value: Option<f32>,
    #[serde(skip)]
    wire_scale_factor: Option<f32>,
    #[serde(skip)]
    wire_offset: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) descriptions: Option<HashMap<i32, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) valid_values: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "is_false")]
    is_read_only: bool,
    #[serde(skip)]
    is_send_always: bool, // Whether the controlvalue is sent out to client in all state messages
    #[serde(skip)]
    destination: ControlDestination,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ControlDefinition {}

#[derive(Error, Debug)]
pub enum ControlError {
    #[error("Control {0} not supported on this radar")]
    NotSupported(String),
    #[error("Control {0} value {1} is lower than minimum value {2}")]
    TooLow(String, f32, f32),
    #[error("Control {0} value {1} is higher than maximum value {2}")]
    TooHigh(String, f32, f32),
    #[error("Control {0} value {1} is not a legal value")]
    Invalid(String, String),
    #[error("Control {0} does not support Auto")]
    NoAuto(String),
    #[error("Control {0} value '{1}' requires true heading input")]
    NoHeading(String, &'static str),
    #[error("Control {0} value '{1}' requires a GNSS position")]
    NoPosition(String, &'static str),
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn serialize_control_value() {
        // Using string IDs now (SignalK camelCase format)
        let json = r#"{"id":"gain","value":"49","auto":true,"enabled":false}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, "gain");
                assert_eq!(cv.value, "49");
                assert_eq!(cv.auto, Some(true));
                assert_eq!(cv.enabled, Some(false));
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }
        let json = r#"{"id":"gain","value":"49"}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, "gain");
                assert_eq!(cv.value, "49");
                assert_eq!(cv.auto, None);
                assert_eq!(cv.enabled, None);
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }
    }

    #[test]
    fn control_range_values() {
        let session = crate::Session::new_fake();
        let controls = SharedControls::new(session, HashMap::new());

        assert!(controls.set("targetTrails", 0., None).is_ok());
        assert_eq!(controls.set("targetTrails", 6., None).unwrap(), Some(()));
        assert!(controls.set("targetTrails", 7., None).is_err());
        assert!(controls.set("targetTrails", -1., None).is_err());
        assert!(controls.set("targetTrails", 0.3, None).is_ok());
    }
}
