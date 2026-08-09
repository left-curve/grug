"""Tests for dango.info.Info — typed perps smart-contract queries (Phase 7)."""

from __future__ import annotations

import json
from typing import Any, cast

from pytest_httpserver import HTTPServer
from werkzeug.wrappers import Request, Response

from dango.info import Info
from dango.utils.constants import PERPS_CONTRACT_MAINNET, PERPS_CONTRACT_TESTNET
from dango.utils.types import Addr, OrderId, PairId

# A throw-away user address used by every test that needs one. The exact
# bytes don't matter — the contract never sees them since we stub the
# transport. Using a recognizable hex tail (`...beef`) makes failed
# assertions easier to read in test output.
_DEMO_USER = Addr("0x000000000000000000000000000000000000beef")
_DEMO_PAIR = PairId("perp/btcusd")
_DEMO_ORDER_ID = OrderId("42")


def _info(httpserver: HTTPServer) -> Info:
    """Build an Info bound to the local httpserver. Uses the default mainnet contract."""

    # We intentionally don't override `perps_contract` here so the wire
    # assertions against `PERPS_CONTRACT_MAINNET` exercise the default path.
    return Info(httpserver.url_for("/").rstrip("/"))


def _capture_request(
    httpserver: HTTPServer,
    response: dict[str, Any],
) -> list[dict[str, Any]]:
    """Stub /graphql to return `response` and capture each inbound JSON body.

    Mirrors the Phase 6 helper from `test_info_primitives.py` so the test
    surface stays consistent across phases. Returning the captured-bodies
    list lets the caller assert on the on-the-wire query / variables shape
    after invoking the SDK method under test.
    """

    captured: list[dict[str, Any]] = []

    def handler(request: Request) -> Response:
        captured.append(cast("dict[str, Any]", request.get_json()))
        return Response(json.dumps(response), mimetype="application/json")

    httpserver.expect_request("/graphql", method="POST").respond_with_handler(handler)
    return captured


def _wasm_smart_msg(captured: dict[str, Any]) -> dict[str, Any]:
    """Drill into the captured GraphQL body and return the wasm_smart `msg` dict."""

    # The wire body is `{query, variables: {request: {wasm_smart: {contract, msg}}, height}}`
    # so we navigate through it once and reuse the helper across many tests.
    request = captured["variables"]["request"]
    return cast("dict[str, Any]", request["wasm_smart"]["msg"])


def _wasm_smart_contract(captured: dict[str, Any]) -> str:
    """Drill into the captured GraphQL body and return the wasm_smart `contract` address."""

    request = captured["variables"]["request"]
    return cast("str", request["wasm_smart"]["contract"])


# --- Constructor -------------------------------------------------------------


class TestConstructor:
    def test_default_perps_contract_is_mainnet(self) -> None:
        """Omitting `perps_contract` defaults to the mainnet address constant."""

        info = Info("http://example.com")
        assert info.perps_contract == PERPS_CONTRACT_MAINNET

    def test_can_override_perps_contract(self) -> None:
        """Passing `perps_contract=` (e.g. testnet) overrides the default."""

        info = Info("http://example.com", perps_contract=Addr(PERPS_CONTRACT_TESTNET))
        assert info.perps_contract == PERPS_CONTRACT_TESTNET


# --- Global queries ----------------------------------------------------------


class TestGlobalQueries:
    def test_perps_param(self, httpserver: HTTPServer) -> None:
        """`perps_param()` posts `{param: {}}` and returns the typed payload."""

        param_payload = {
            "max_unlocks": 5,
            "max_open_orders": 50,
            "maker_fee_rates": {"base": "0.000000", "tiers": {}},
            "taker_fee_rates": {"base": "0.001000", "tiers": {}},
            "protocol_fee_rate": "0.100000",
            "liquidation_fee_rate": "0.010000",
            "liquidation_buffer_ratio": "0.000000",
            "funding_period": "3600000000000",
            "vault_total_weight": "10.000000",
            "vault_cooldown_period": "604800000000000",
            "referral_active": True,
            "min_referrer_volume": "0.000000",
            "referrer_commission_rates": {"base": "0.000000", "tiers": {}},
            "vault_deposit_cap": None,
            "max_action_batch_size": 5,
            "trading_enabled": True,
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": param_payload}}}
        )
        result = _info(httpserver).perps_param()
        assert "QueryApp" in captured[0]["query"]
        assert _wasm_smart_contract(captured[0]) == PERPS_CONTRACT_MAINNET
        assert _wasm_smart_msg(captured[0]) == {"param": {}}
        assert result == param_payload

    def test_perps_state(self, httpserver: HTTPServer) -> None:
        """`perps_state()` posts `{state: {}}` and returns the typed payload."""

        state_payload = {
            "last_funding_time": "1700000000000000000",
            "vault_share_supply": "500000000",
            "insurance_fund": "25000.000000",
            "treasury": "12000.000000",
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": state_payload}}}
        )
        result = _info(httpserver).perps_state()
        assert _wasm_smart_msg(captured[0]) == {"state": {}}
        assert result == state_payload


# --- Pair-level queries ------------------------------------------------------


class TestPairQueries:
    def test_pair_param(self, httpserver: HTTPServer) -> None:
        """`pair_param(pair_id)` posts `{pair_param: {pair_id}}`."""

        pair_payload = {
            "tick_size": "1.000000",
            "min_order_size": "10.000000",
            "max_limit_price_deviation": "0.100000",
            "max_market_slippage": "0.100000",
            "max_abs_oi": "1000000.000000",
            "max_abs_funding_rate": "0.000500",
            "initial_margin_ratio": "0.050000",
            "maintenance_margin_ratio": "0.025000",
            "impact_size": "10000.000000",
            "vault_liquidity_weight": "1.000000",
            "vault_half_spread": "0.001000",
            "vault_max_quote_size": "50000.000000",
            "vault_size_skew_factor": "0.000000",
            "vault_spread_skew_factor": "0.000000",
            "vault_max_skew_size": "0.000000",
            "funding_rate_multiplier": "1.000000",
            "bucket_sizes": ["1.000000", "5.000000", "10.000000"],
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": pair_payload}}}
        )
        result = _info(httpserver).pair_param(_DEMO_PAIR)
        assert _wasm_smart_msg(captured[0]) == {"pair_param": {"pair_id": _DEMO_PAIR}}
        assert result == pair_payload

    def test_pair_param_returns_none_for_missing_pair(self, httpserver: HTTPServer) -> None:
        """Contract returns `null` for an unconfigured pair; SDK surfaces `None`."""

        _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": None}}})
        assert _info(httpserver).pair_param(PairId("perp/unknown")) is None

    def test_pair_params_pagination(self, httpserver: HTTPServer) -> None:
        """`pair_params(start_after=, limit=)` forwards both knobs verbatim."""

        captured = _capture_request(
            httpserver,
            {"data": {"queryApp": {"wasm_smart": {_DEMO_PAIR: {}}}}},
        )
        _info(httpserver).pair_params(start_after=_DEMO_PAIR, limit=10)
        assert _wasm_smart_msg(captured[0]) == {
            "pair_params": {"start_after": _DEMO_PAIR, "limit": 10},
        }

    def test_pair_params_defaults_send_null_start_after_and_limit_30(
        self,
        httpserver: HTTPServer,
    ) -> None:
        """Default kwargs send `start_after=null` and `limit=30` (the roadmap default)."""

        captured = _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": {}}}})
        _info(httpserver).pair_params()
        assert _wasm_smart_msg(captured[0]) == {
            "pair_params": {"start_after": None, "limit": 30},
        }

    def test_pair_state(self, httpserver: HTTPServer) -> None:
        """`pair_state(pair_id)` posts `{pair_state: {pair_id}}`."""

        state_payload = {
            "long_oi": "12500.000000",
            "short_oi": "10300.000000",
            "funding_per_unit": "0.000123",
            "funding_rate": "0.000050",
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": state_payload}}}
        )
        result = _info(httpserver).pair_state(_DEMO_PAIR)
        assert _wasm_smart_msg(captured[0]) == {"pair_state": {"pair_id": _DEMO_PAIR}}
        assert result == state_payload

    def test_pair_state_returns_none_for_missing_pair(self, httpserver: HTTPServer) -> None:
        """Unconfigured pair => `null` => `None`."""

        _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": None}}})
        assert _info(httpserver).pair_state(PairId("perp/unknown")) is None

    def test_pair_states_pagination(self, httpserver: HTTPServer) -> None:
        """`pair_states(start_after=, limit=)` forwards both knobs verbatim."""

        captured = _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": {}}}})
        _info(httpserver).pair_states(start_after=_DEMO_PAIR, limit=5)
        assert _wasm_smart_msg(captured[0]) == {
            "pair_states": {"start_after": _DEMO_PAIR, "limit": 5},
        }


# --- Liquidity depth ---------------------------------------------------------


class TestLiquidityDepth:
    def test_passes_pair_id_bucket_size_limit(self, httpserver: HTTPServer) -> None:
        """All three positional/keyword args land on the wire as snake_case."""

        depth_payload = {
            "bids": {
                "64990.000000": {"size": "12.500000", "notional": "812375.000000"},
            },
            "asks": {
                "65010.000000": {"size": "10.000000", "notional": "650100.000000"},
            },
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": depth_payload}}}
        )
        result = _info(httpserver).liquidity_depth(
            _DEMO_PAIR,
            bucket_size="10.000000",
            limit=20,
        )
        assert _wasm_smart_msg(captured[0]) == {
            "liquidity_depth": {
                "pair_id": _DEMO_PAIR,
                "bucket_size": "10.000000",
                "limit": 20,
            },
        }
        assert result == depth_payload

    def test_omitting_limit_sends_null(self, httpserver: HTTPServer) -> None:
        """`limit=None` (the default) hits the wire as JSON null, not a missing field."""

        # The Rust side has `limit: Option<u32>`; serde happily accepts `null`.
        # We test for the explicit-null path because the SDK builds the dict
        # eagerly rather than conditionally including `limit` only when set —
        # which means downstream JSON inspection always sees the key.
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": {"bids": {}, "asks": {}}}}}
        )
        _info(httpserver).liquidity_depth(_DEMO_PAIR, bucket_size="10.000000")
        msg = _wasm_smart_msg(captured[0])
        assert msg["liquidity_depth"]["limit"] is None


# --- User queries ------------------------------------------------------------


class TestUserQueries:
    def test_user_state(self, httpserver: HTTPServer) -> None:
        """`user_state(addr)` posts `{user_state: {user}}` and returns the payload."""

        state_payload = {
            "margin": "10000.000000",
            "vault_shares": "0",
            "positions": {},
            "unlocks": [],
            "reserved_margin": "0.000000",
            "open_order_count": 0,
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": state_payload}}}
        )
        result = _info(httpserver).user_state(_DEMO_USER)
        assert _wasm_smart_msg(captured[0]) == {"user_state": {"user": _DEMO_USER}}
        assert result == state_payload

    def test_user_state_returns_none_for_unknown_user(self, httpserver: HTTPServer) -> None:
        """Unknown user => contract returns `null` => SDK surfaces `None`."""

        _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": None}}})
        assert _info(httpserver).user_state(_DEMO_USER) is None

    def test_user_state_extended_default_knobs(self, httpserver: HTTPServer) -> None:
        """Default knobs match the roadmap: 5 true, `include_liquidation_price=False`."""

        captured = _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": {}}}})
        _info(httpserver).user_state_extended(_DEMO_USER)
        msg = _wasm_smart_msg(captured[0])
        assert msg == {
            "user_state_extended": {
                "user": _DEMO_USER,
                "include_equity": True,
                "include_available_margin": True,
                "include_maintenance_margin": True,
                "include_unrealized_pnl": True,
                "include_unrealized_funding": True,
                "include_liquidation_price": False,
            },
        }

    def test_user_state_extended_overridden_knobs(self, httpserver: HTTPServer) -> None:
        """Each knob is independently overridable via kwargs."""

        captured = _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": {}}}})
        _info(httpserver).user_state_extended(
            _DEMO_USER,
            include_equity=False,
            include_available_margin=False,
            include_maintenance_margin=False,
            include_unrealized_pnl=False,
            include_unrealized_funding=False,
            include_liquidation_price=True,
        )
        msg = _wasm_smart_msg(captured[0])
        assert msg["user_state_extended"]["include_equity"] is False
        assert msg["user_state_extended"]["include_available_margin"] is False
        assert msg["user_state_extended"]["include_maintenance_margin"] is False
        assert msg["user_state_extended"]["include_unrealized_pnl"] is False
        assert msg["user_state_extended"]["include_unrealized_funding"] is False
        assert msg["user_state_extended"]["include_liquidation_price"] is True

    def test_user_state_extended_does_not_send_include_all(self, httpserver: HTTPServer) -> None:
        """The Rust-side `include_all` flag is intentionally not exposed; never on the wire."""

        # Pinning this design decision: `include_all` defaults to false via
        # serde, and exposing it on the Python signature would create two
        # ways to say the same thing. The wire body must never carry it,
        # whether the user used default knobs or overrides.
        captured = _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": {}}}})
        _info(httpserver).user_state_extended(_DEMO_USER, include_liquidation_price=True)
        msg = _wasm_smart_msg(captured[0])
        assert "include_all" not in msg["user_state_extended"]


# --- Order queries -----------------------------------------------------------


class TestOrderQueries:
    def test_orders_by_user(self, httpserver: HTTPServer) -> None:
        """`orders_by_user(addr)` posts `{orders_by_user: {user}}`."""

        orders_payload = {
            "42": {
                "pair_id": _DEMO_PAIR,
                "size": "0.500000",
                "limit_price": "63000.000000",
                "reduce_only": False,
                "reserved_margin": "1575.000000",
                "created_at": "1700000000000000000",
            },
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": orders_payload}}}
        )
        result = _info(httpserver).orders_by_user(_DEMO_USER)
        assert _wasm_smart_msg(captured[0]) == {"orders_by_user": {"user": _DEMO_USER}}
        assert result == orders_payload

    def test_order_found(self, httpserver: HTTPServer) -> None:
        """`order(order_id)` posts `{order: {order_id}}` and returns the payload."""

        order_payload = {
            "user": _DEMO_USER,
            "pair_id": _DEMO_PAIR,
            "size": "0.500000",
            "limit_price": "63000.000000",
            "reduce_only": False,
            "reserved_margin": "1575.000000",
            "created_at": "1700000000000000000",
        }
        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": order_payload}}}
        )
        result = _info(httpserver).order(_DEMO_ORDER_ID)
        assert _wasm_smart_msg(captured[0]) == {"order": {"order_id": _DEMO_ORDER_ID}}
        assert result == order_payload

    def test_order_returns_none_for_missing(self, httpserver: HTTPServer) -> None:
        """Missing order => `null` => `None`."""

        _capture_request(httpserver, {"data": {"queryApp": {"wasm_smart": None}}})
        assert _info(httpserver).order(_DEMO_ORDER_ID) is None


# --- Volume ------------------------------------------------------------------


class TestVolume:
    def test_lifetime_volume(self, httpserver: HTTPServer) -> None:
        """`since=None` (default) sends `since: null` and returns the UsdValue string."""

        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": "1250000.000000"}}}
        )
        result = _info(httpserver).volume(_DEMO_USER)
        assert _wasm_smart_msg(captured[0]) == {
            "volume": {"user": _DEMO_USER, "since": None},
        }
        # The contract returns a 6-decimal UsdValue string; we surface it
        # verbatim. Asserting on the precise string proves no parse-and-
        # reformat round-trip is silently happening.
        assert result == "1250000.000000"

    def test_volume_since(self, httpserver: HTTPServer) -> None:
        """An explicit `since` timestamp is forwarded as an integer in the request."""

        captured = _capture_request(
            httpserver, {"data": {"queryApp": {"wasm_smart": "500.000000"}}}
        )
        result = _info(httpserver).volume(_DEMO_USER, since=1700000000000000000)
        assert _wasm_smart_msg(captured[0]) == {
            "volume": {"user": _DEMO_USER, "since": 1700000000000000000},
        }
        assert result == "500.000000"
