mod bank;
mod perps;

use {
    dango_app::AppResult,
    dango_primitives::{BlockInfo, Storage},
};

pub fn do_upgrade<VM>(storage: Box<dyn Storage>, _vm: VM, _block: BlockInfo) -> AppResult<()> {
    bank::do_bank_upgrades(storage)?;

    Ok(())
}
