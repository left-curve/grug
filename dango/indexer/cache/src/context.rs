use {
    crate::IndexerPath,
    dango_primitives::TransactionsHttpdRequest,
    std::sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub struct Context {
    pub transactions_http_request_details: Arc<Mutex<TransactionsHttpdRequest>>,
    pub indexer_path: IndexerPath,
}
