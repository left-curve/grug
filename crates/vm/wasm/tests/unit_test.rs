use {
    colored::*,
    grug_app::{GasTracker, Instance, QuerierProvider, Shared, StorageProvider, Vm},
    grug_crypto::sha2_256,
    grug_tester_benchmarker::{ExecuteMsg, QueryMsg},
    grug_types::{
        from_json_slice, to_json_vec, Addr, BlockInfo, Coins, Context, GenericResult, Hash, Json,
        MockStorage, Order, Timestamp, Uint128, Uint64,
    },
    grug_vm_wasm::WasmVm,
};

const MOCK_CHAIN_ID: &str = "dev-1";

const MOCK_CONTRACT: Addr = Addr::mock(1);

const MOCK_BLOCK: BlockInfo = BlockInfo {
    height: Uint64::new(1),
    timestamp: Timestamp::from_seconds(100),
    hash: Hash::ZERO,
};

static BENCHMARKER_CODE: &[u8] = include_bytes!("../testdata/grug_tester_benchmarker.wasm");

fn setup(
    vm: &mut WasmVm,
    storage: Option<Shared<MockStorage>>,
    gas_tracker: Option<GasTracker>,
) -> anyhow::Result<(
    grug_vm_wasm::WasmInstance,
    Context,
    GasTracker,
    Shared<MockStorage>,
)> {
    let storage = storage.unwrap_or_default();
    let gas_tracker = gas_tracker.unwrap_or_else(GasTracker::new_limitless);

    let querier = QuerierProvider::new(
        vm.clone(),
        Box::new(storage.clone()),
        gas_tracker.clone(),
        MOCK_BLOCK,
    );
    let storage_provider = StorageProvider::new(Box::new(storage.clone()), &[&MOCK_CONTRACT]);

    let instance = vm.build_instance(
        BENCHMARKER_CODE,
        &Hash::from_slice(sha2_256(BENCHMARKER_CODE)),
        storage_provider,
        false,
        querier,
        gas_tracker.clone(),
    )?;

    let ctx = Context {
        chain_id: MOCK_CHAIN_ID.to_string(),
        block: MOCK_BLOCK,
        contract: MOCK_CONTRACT,
        sender: Some(Addr::mock(3)),
        funds: Some(Coins::default()),
        simulate: Some(false),
    };

    Ok((instance, ctx, gas_tracker, storage))
}

const LIMIT: u32 = 200;

const ORDER: Order = Order::Ascending;

fn scan_execute(sized: bool) -> Json {
    let mut vm = WasmVm::new(10000);

    let (instance, ctx, _, storage) = setup(&mut vm, None, None).unwrap();

    let data = (1..LIMIT + 1).fold(vec![], |mut buf, i| {
        buf.push((i.to_string(), Uint128::from(i as u128)));
        buf
    });

    let msg = to_json_vec(&ExecuteMsg::Populate { data }).unwrap();

    let res = instance.call_in_1_out_1("execute", &ctx, &msg).unwrap();

    from_json_slice::<GenericResult<Json>>(res).unwrap().as_ok();

    let (instance, ctx, ..) = setup(&mut vm, Some(storage), None).unwrap();

    let query_msg = to_json_vec(&QueryMsg::Data {
        min: None,
        max: None,
        order: ORDER,
        limit: LIMIT,
        sized,
    })
    .unwrap();

    let res = instance.call_in_1_out_1("query", &ctx, &query_msg).unwrap();

    from_json_slice::<GenericResult<Json>>(&res)
        .unwrap()
        .as_ok()
}

#[test]
fn scan_sized_vs_non_sized() {
    let sized = scan_execute(true);
    let non_sized = scan_execute(false);

    match (&non_sized, &sized) {
        (Json::Array(non_sized), Json::Array(sized)) => {
            if non_sized != sized {
                let clos = |comp: &[Json], with: &[Json], desc: &str| {
                    println!("{desc} - len: {}", comp.len());
                    for i in comp {
                        if !with.contains(i) {
                            print!("{}", format!("{i},").red());
                        } else {
                            print!("{}", format!("{i},").black());
                        }
                    }
                    println!();
                };

                println!("Result as differents! - iterations: {}", LIMIT);
                clos(non_sized, sized, "non_sized");
                clos(sized, non_sized, "sized");
            } else {
                println!("Both results are equal");
            }
        },
        _ => panic!("unexpected output format"),
    }
}
