use {
    crate::context::FullContext,
    actix_web::{
        Error, HttpResponse, Scope,
        error::{ErrorBadRequest, ErrorInternalServerError},
        get, web,
    },
    dango_indexer_cache::cache_file::CacheFile,
    dango_primitives::FullBlock,
    serde::Deserialize,
    std::path::PathBuf,
    utoipa::IntoParams,
};

/// Maximum number of blocks returned by `/block/full/range` in one request.
const MAX_FULL_BLOCK_RANGE: u64 = 20;

pub fn services() -> Scope {
    web::scope("/block")
        .service(block_info_by_height)
        .service(latest_block_info)
        .service(block_result_by_height)
        .service(block_result)
        // `/full/range` is registered before `/full/{block_height}` so the
        // literal "range" segment is not captured as a block height.
        .service(full_block_range)
        .service(full_block_by_height)
        .service(latest_full_block)
}

#[utoipa::path(
    get,
    path = "/block/info",
    tag = "block",
    summary = "Latest block info",
    description = "Metadata and transactions of the latest finalized block, \
                   read from the node's block cache.",
    responses(
        (status = 200, description = "The latest finalized block (metadata + transactions)"),
        (status = 404, description = "The latest block's file is not in the cache yet"),
        (status = 500, description = "The chain query failed or the block file could not be read"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/info")]
pub async fn latest_block_info(app_ctx: web::Data<FullContext>) -> Result<HttpResponse, Error> {
    let block_height = app_ctx
        .dango_app()
        .last_finalized_block()
        .await
        .map_err(ErrorInternalServerError)?
        .height;

    _block_by_height(block_height, &app_ctx)
}

#[utoipa::path(
    get,
    path = "/block/info/{block_height}",
    tag = "block",
    summary = "Block info by height",
    description = "Metadata and transactions of the block at `block_height`, \
                   read from the node's block cache.",
    params(
        ("block_height" = u64, Path, description = "Block height"),
    ),
    responses(
        (status = 200, description = "The block at that height (metadata + transactions)"),
        (status = 404, description = "No block at that height in the cache"),
        (status = 500, description = "The block file could not be read"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/info/{block_height}")]
pub async fn block_info_by_height(
    path: web::Path<u64>,
    app_ctx: web::Data<FullContext>,
) -> Result<HttpResponse, Error> {
    _block_by_height(path.into_inner(), &app_ctx)
}

fn _block_by_height(block_height: u64, app_ctx: &FullContext) -> Result<HttpResponse, Error> {
    let block_filename = app_ctx
        .indexer_cache_context
        .indexer_path
        .block_path(block_height);

    check_block_exists(block_filename.clone(), block_height)?;

    match CacheFile::load_from_disk(block_filename) {
        Ok(cache_file) => Ok(HttpResponse::Ok().json(cache_file.data.block)),
        Err(err) => {
            Ok(HttpResponse::InternalServerError()
                .body(format!("failed to load block file: {err}")))
        },
    }
}

#[utoipa::path(
    get,
    path = "/block/result",
    tag = "block",
    summary = "Latest block outcome",
    description = "Execution outcome of the latest finalized block, read from \
                   the node's block cache.",
    responses(
        (status = 200, description = "The latest finalized block's outcome"),
        (status = 404, description = "The latest block's file is not in the cache yet"),
        (status = 500, description = "The chain query failed or the block file could not be read"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/result")]
pub async fn block_result(app_ctx: web::Data<FullContext>) -> Result<HttpResponse, Error> {
    let block_height = app_ctx
        .dango_app()
        .last_finalized_block()
        .await
        .map_err(ErrorInternalServerError)?
        .height;

    _block_results_by_height(block_height, &app_ctx)
}

#[utoipa::path(
    get,
    path = "/block/result/{block_height}",
    tag = "block",
    summary = "Block outcome by height",
    description = "Execution outcome of the block at `block_height`, read from \
                   the node's block cache.",
    params(
        ("block_height" = u64, Path, description = "Block height"),
    ),
    responses(
        (status = 200, description = "The block's outcome"),
        (status = 404, description = "No block at that height in the cache"),
        (status = 500, description = "The block file could not be read"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/result/{block_height}")]
pub async fn block_result_by_height(
    path: web::Path<u64>,
    app_ctx: web::Data<FullContext>,
) -> Result<HttpResponse, Error> {
    _block_results_by_height(path.into_inner(), &app_ctx)
}

fn _block_results_by_height(
    block_height: u64,
    app_ctx: &FullContext,
) -> Result<HttpResponse, Error> {
    let block_filename = app_ctx
        .indexer_cache_context
        .indexer_path
        .block_path(block_height);

    check_block_exists(block_filename.clone(), block_height)?;

    match CacheFile::load_from_disk(block_filename) {
        Ok(cache_file) => Ok(HttpResponse::Ok().json(cache_file.data.block_outcome)),
        Err(err) => {
            Ok(HttpResponse::InternalServerError()
                .body(format!("failed to load block file: {err}")))
        },
    }
}

// ---- /full: block info + outcome together ----

/// The latest finalized block — both its info and its outcome.
#[utoipa::path(
    get,
    path = "/block/full",
    tag = "block",
    summary = "Latest full block",
    description = "The latest finalized block as `{ block, outcome }` — info \
                   and execution outcome together, read from the node's block \
                   cache.",
    responses(
        (status = 200, description = "The latest full block (`{ block, outcome }`)"),
        (status = 404, description = "The latest block's file is not in the cache yet"),
        (status = 500, description = "The chain query failed or the block file could not be read"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/full")]
pub async fn latest_full_block(app_ctx: web::Data<FullContext>) -> Result<HttpResponse, Error> {
    let block_height = app_ctx
        .dango_app()
        .last_finalized_block()
        .await
        .map_err(ErrorInternalServerError)?
        .height;

    _full_block_by_height(block_height, &app_ctx)
}

/// A specific block by height — both its info and its outcome.
#[utoipa::path(
    get,
    path = "/block/full/{block_height}",
    tag = "block",
    summary = "Full block by height",
    description = "The block at `block_height` as `{ block, outcome }` — the \
                   same `FullBlock` shape the `/ws` `fullBlock` channel \
                   streams.",
    params(
        ("block_height" = u64, Path, description = "Block height"),
    ),
    responses(
        (status = 200, description = "The full block (`{ block, outcome }`)"),
        (status = 404, description = "No block at that height in the cache"),
        (status = 500, description = "The block file could not be read"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/full/{block_height}")]
pub async fn full_block_by_height(
    path: web::Path<u64>,
    app_ctx: web::Data<FullContext>,
) -> Result<HttpResponse, Error> {
    _full_block_by_height(path.into_inner(), &app_ctx)
}

/// A contiguous chunk of full blocks starting at `from`, through `to`
/// (inclusive), capped at [`MAX_FULL_BLOCK_RANGE`]. Iteration stops at the first
/// height with no block on disk, so the result is always a gap-free run from
/// `from`: a range past the chain tip returns the blocks up to the tip, and if
/// `from` itself is missing the result is empty.
#[utoipa::path(
    get,
    path = "/block/full/range",
    tag = "block",
    summary = "Contiguous run of full blocks",
    description = "Full blocks from `from` through `to` (inclusive), capped at \
                   20 per request. Iteration stops at the first height with no \
                   block in the cache, so the result is always a gap-free run \
                   from `from`: a range past the chain tip returns the blocks \
                   up to the tip, and if `from` itself is missing the result \
                   is empty.",
    params(RangeQuery),
    responses(
        (status = 200, description = "A gap-free run of full blocks from `from` (possibly empty)"),
        (status = 400, description = "`from` greater than `to`"),
        (status = 500, description = "A block file could not be read"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/full/range")]
pub async fn full_block_range(
    query: web::Query<RangeQuery>,
    app_ctx: web::Data<FullContext>,
) -> Result<HttpResponse, Error> {
    let RangeQuery { from, to } = query.into_inner();

    if from > to {
        return Err(ErrorBadRequest(format!(
            "invalid range: from ({from}) must be <= to ({to})"
        )));
    }

    let to = to.min(from.saturating_add(MAX_FULL_BLOCK_RANGE - 1));

    let mut blocks = Vec::with_capacity((to - from + 1) as usize);
    for block_height in from..=to {
        let block_filename = app_ctx
            .indexer_cache_context
            .indexer_path
            .block_path(block_height);

        if CacheFile::exists(block_filename.clone()) {
            blocks.push(load_full_block(block_filename)?);
        } else {
            break;
        }
    }

    Ok(HttpResponse::Ok().json(blocks))
}

fn _full_block_by_height(block_height: u64, app_ctx: &FullContext) -> Result<HttpResponse, Error> {
    let block_filename = app_ctx
        .indexer_cache_context
        .indexer_path
        .block_path(block_height);

    check_block_exists(block_filename.clone(), block_height)?;

    Ok(HttpResponse::Ok().json(load_full_block(block_filename)?))
}

/// Load a block file from disk and project it to `block` + `block_outcome`,
/// dropping the `http_request_details` (client IPs) the file also holds — the
/// same projection the `/info` and `/result` routes use. Returns the same
/// `FullBlock` shape as the `/ws` `fullBlock` channel.
fn load_full_block(block_filename: PathBuf) -> Result<FullBlock, Error> {
    let cache_file = CacheFile::load_from_disk(block_filename)
        .map_err(|err| ErrorInternalServerError(format!("failed to load block file: {err}")))?;

    Ok(FullBlock {
        block: cache_file.data.block,
        outcome: cache_file.data.block_outcome,
    })
}

/// `/block/full/range` arguments.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct RangeQuery {
    /// First block height, inclusive.
    from: u64,
    /// Last block height, inclusive; the span is capped at 20 blocks
    /// (clamped to `from + 19`).
    to: u64,
}

fn check_block_exists(block_filename: PathBuf, height: u64) -> Result<(), Error> {
    if !CacheFile::exists(block_filename) {
        Err(actix_web::error::ErrorNotFound(format!(
            "block not found: {height}",
        )))
    } else {
        Ok(())
    }
}
