use {clap::Parser, grug_app::App, grug_db_disk::DiskDb, grug_vm_wasm::WasmVm, std::path::PathBuf};

#[derive(Parser)]
pub struct StartCmd {
    /// Tendermint ABCI listening address
    #[arg(long, default_value = "127.0.0.1:26658")]
    abci_addr: String,

    /// Size of the read buffer for each incoming connection to the ABCI server, in bytes
    #[arg(long, default_value = "1048576")]
    read_buf_size: usize,

    /// Capacity of the wasm module cache; zero means do not use a cache
    #[arg(long, default_value = "1000")]
    wasm_cache_capacity: usize,

    /// Gas limit when serving query requests [default: u64::MAX]
    #[arg(long)]
    query_gas_limit: Option<u64>,
}

impl StartCmd {
    pub async fn run(self, data_dir: PathBuf) -> anyhow::Result<()> {
        let db = DiskDb::open(data_dir)?;
        let vm = WasmVm::new(self.wasm_cache_capacity);
        let app = App::new(db, vm, self.query_gas_limit);

        Ok(app.start_abci_server(self.read_buf_size, self.abci_addr)?)
    }
}
