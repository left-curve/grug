#[cfg(feature = "tracing")]
use dango_dyn_event::dyn_event;
use {
    crate::{
        APP_CONFIG, AppError, AppResult, CONFIG, EventResult, NEXT_CRONJOBS, TraceOption,
        schedule_cronjob,
    },
    dango_primitives::{Addr, BlockInfo, Config, EvtConfigure, JsonDeExt, MsgConfigure, Storage},
};

pub fn do_configure(
    storage: &mut dyn Storage,
    block: BlockInfo,
    sender: Addr,
    msg: MsgConfigure,
    #[allow(unused_variables)] trace_opt: TraceOption,
) -> EventResult<EvtConfigure> {
    let evt = EvtConfigure { sender };

    match _do_configure(storage, block, sender, msg) {
        Ok(_) => {
            #[cfg(feature = "tracing")]
            dyn_event!(trace_opt.ok_level.into(), "Config updated");

            EventResult::Ok(evt)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            dyn_event!(
                trace_opt.error_level.into(),
                err = err.to_string(),
                "Failed to update config"
            );

            EventResult::err(evt, err)
        },
    }
}

fn _do_configure(
    storage: &mut dyn Storage,
    block: BlockInfo,
    sender: Addr,
    msg: MsgConfigure,
) -> AppResult<()> {
    let cfg = CONFIG.load(storage)?;

    // Make sure the sender is authorized to set the config.
    if sender != cfg.owner {
        return Err(AppError::not_owner(sender, cfg.owner));
    }

    if let Some(new_cfg) = msg.new_cfg {
        // `MsgConfigure.new_cfg` is a raw JSON value rather than a typed
        // `Config`, so that historical blocks containing config updates keep
        // deserializing across `Config` schema changes. Parse it here instead.
        let new_cfg: Config = new_cfg.deserialize_json()?;

        // If the list of cronjobs has been changed, we have to delete the
        // existing scheduled ones and reschedule.
        if new_cfg.cronjobs != cfg.cronjobs {
            NEXT_CRONJOBS.clear(storage, None, None);

            for (contract, interval) in &new_cfg.cronjobs {
                schedule_cronjob(storage, *contract, block.timestamp + *interval)?;
            }
        }

        CONFIG.save(storage, &new_cfg)?;
    }

    if let Some(new_app_cfg) = msg.new_app_cfg {
        APP_CONFIG.save(storage, &new_app_cfg)?;
    }

    Ok(())
}
