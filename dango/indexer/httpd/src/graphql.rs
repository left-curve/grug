#[cfg(feature = "metrics")]
use crate::{
    graphql::extensions::metrics::{MetricsExtension, init_graphql_metrics},
    metrics::init_core_query_metrics,
};
#[cfg(feature = "tracing")]
use async_graphql::extensions as AsyncGraphqlExtensions;
use {
    crate::context::FullContext,
    async_graphql::{Schema, dataloader::DataLoader},
    dango_indexer_sql::dataloaders::{
        block_events::BlockEventsDataLoader, block_transactions::BlockTransactionsDataLoader,
        event_transaction::EventTransactionDataLoader,
        transaction_events::TransactionEventsDataLoader,
        transaction_grug::FileTransactionDataLoader,
        transaction_messages::TransactionMessagesDataLoader,
    },
    telemetry::SentryExtension,
};

pub mod extensions;
pub mod mutation;
pub mod query;
pub mod subscription;
pub mod telemetry;
pub mod types;

pub type FullSchema =
    Schema<query::FullQuery, mutation::IndexerMutation, subscription::FullSubscription>;

pub fn build_full_schema(app_ctx: FullContext) -> FullSchema {
    #[cfg(feature = "metrics")]
    {
        init_graphql_metrics();
        init_core_query_metrics();
    }

    let block_transactions_loader = DataLoader::new(
        BlockTransactionsDataLoader {
            db: app_ctx.db.clone(),
        },
        tokio::spawn,
    );

    let block_events_loader = DataLoader::new(
        BlockEventsDataLoader {
            db: app_ctx.db.clone(),
        },
        tokio::spawn,
    );

    let event_transaction_loader = DataLoader::new(
        EventTransactionDataLoader {
            db: app_ctx.db.clone(),
        },
        tokio::spawn,
    );

    let transaction_messages_loader = DataLoader::new(
        TransactionMessagesDataLoader {
            db: app_ctx.db.clone(),
        },
        tokio::spawn,
    );

    let transaction_events_loader = DataLoader::new(
        TransactionEventsDataLoader {
            db: app_ctx.db.clone(),
        },
        tokio::spawn,
    );

    let file_transaction_loader = DataLoader::new(
        FileTransactionDataLoader {
            indexer_path: app_ctx.indexer_cache_context.indexer_path.clone(),
        },
        tokio::spawn,
    );

    let indexer_path = app_ctx.indexer_cache_context.indexer_path.clone();

    #[allow(unused_mut)]
    let mut schema_builder = Schema::build(
        query::FullQuery::default(),
        mutation::IndexerMutation::default(),
        subscription::FullSubscription::default(),
    )
    .extension(SentryExtension);

    #[cfg(feature = "metrics")]
    {
        schema_builder = schema_builder.extension(MetricsExtension);
    }

    #[cfg(feature = "tracing")]
    {
        schema_builder = schema_builder
            .extension(AsyncGraphqlExtensions::Tracing)
            .extension(AsyncGraphqlExtensions::Logger);
    }

    schema_builder
        .data(app_ctx.clickhouse_context.clone())
        .data(app_ctx.db.clone())
        .data(app_ctx.base.clone())
        .data(app_ctx)
        .data(block_transactions_loader)
        .data(block_events_loader)
        .data(transaction_messages_loader)
        .data(transaction_events_loader)
        .data(file_transaction_loader)
        .data(event_transaction_loader)
        .data(indexer_path)
        .limit_complexity(300)
        .limit_depth(20)
        .finish()
}
