use {
    dango_app::AppResult,
    dango_primitives::{BlockInfo, Storage},
};

pub fn do_upgrade<VM>(_storage: Box<dyn Storage>, _vm: VM, _block: BlockInfo) -> AppResult<()> {
    tracing::info!("Nothing to do in this upgrade");

    Ok(())
}
