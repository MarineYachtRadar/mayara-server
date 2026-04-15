use async_trait::async_trait;

use crate::brand::CommandSender;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{RadarError, RadarInfo};

/// Command sender for the emulator radar.
/// Since this is a simulated radar, most commands are just logged.
pub(crate) struct Command {
    key: String,
}

impl Command {
    pub(crate) fn new(info: RadarInfo) -> Self {
        Command { key: info.key() }
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        log::debug!(
            "{}: Emulator set_control {:?} = {:?}",
            self.key,
            cv.id,
            cv.value
        );
        // RangeUnits is a client-side display preference; persist it in
        // SharedControls since no emulator state loop echoes it back.
        if cv.id == ControlId::RangeUnits {
            if let Some(v) = cv.value.as_ref().and_then(|v| v.as_f64()) {
                controls
                    .set_value(&ControlId::RangeUnits, v.into())
                    .map_err(RadarError::ControlError)?;
            }
        }
        // Emulator just acknowledges the command - actual state is managed in report.rs
        Ok(())
    }
}
