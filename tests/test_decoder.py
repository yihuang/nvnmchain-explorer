"""Unit tests for decoder: flatten_trace, extract_balance_changes, extract_state_changes, extract_calls."""

from __future__ import annotations

from eth_utils import to_checksum_address


def _transfer_log(from_addr: str, to_addr: str, amount: int, contract: str | None = None):
    """Build a minimal Transfer event log dict."""
    contract = contract or "0x" + "c" * 40
    return {
        "address": contract,
        "topics": [
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
            "0x" + "00" * 12 + from_addr[2:],
            "0x" + "00" * 12 + to_addr[2:],
        ],
        "data": "0x" + format(amount, "064x"),
        "logIndex": "0x0",
    }


def _transfer_calldata(to_addr: str, amount: int) -> str:
    """ABI-encode transfer(address,uint256) calldata."""
    addr_padded = "000000000000000000000000" + to_addr[2:].zfill(40)
    amount_hex = format(amount, "064x")
    return "0xa9059cbb" + addr_padded + amount_hex


# ── flatten_trace ───────────────────────────────────────────────────


def test_flatten_trace_none():
    from app.decoder import flatten_trace

    assert flatten_trace(None) == []
    assert flatten_trace({}) == []


def test_flatten_trace_single_call():
    from app.decoder import flatten_trace

    trace = {
        "type": "CALL",
        "from": "0x1111111111111111111111111111111111111111",
        "to": "0x2222222222222222222222222222222222222222",
        "input": _transfer_calldata("0x0000000000000000000000000000000000000001", 100),
        "output": "0x",
        "value": "0x0",
        "gas": "0x5208",
        "gasUsed": "0x1000",
    }
    flat = flatten_trace(trace)
    assert len(flat) == 1
    assert flat[0]["depth"] == 0
    assert flat[0]["type"] == "CALL"
    assert flat[0]["from"] == to_checksum_address("0x1111111111111111111111111111111111111111")
    assert flat[0]["to"] == to_checksum_address("0x2222222222222222222222222222222222222222")
    assert flat[0]["gas"] == "21000"
    assert flat[0]["gas_used"] == "4096"
    assert flat[0]["value"] == "0"
    assert flat[0]["decoded"]["name"] == "transfer"


def test_flatten_trace_nested():
    from app.decoder import flatten_trace

    A = "0x" + "a" * 40
    B = "0x" + "b" * 40
    C = "0x" + "c" * 40
    D = "0x" + "d" * 40
    trace = {
        "type": "CALL",
        "from": A,
        "to": B,
        "input": "0x",
        "value": "0x0",
        "gas": "0x186a0",
        "gasUsed": "0xcccc",
        "calls": [
            {
                "type": "STATICCALL",
                "from": B,
                "to": C,
                "input": "0x",
                "value": "0x0",
                "gas": "0x5208",
                "gasUsed": "0x100",
            },
            {
                "type": "CALL",
                "from": B,
                "to": D,
                "input": "0x",
                "value": "0x01",
                "gas": "0x7530",
                "gasUsed": "0x2000",
            },
        ],
    }
    flat = flatten_trace(trace)
    assert len(flat) == 3
    assert flat[0]["depth"] == 0
    assert flat[1]["depth"] == 1
    assert flat[1]["type"] == "STATICCALL"
    assert flat[2]["depth"] == 1
    assert flat[2]["type"] == "CALL"
    assert len(flat[0]["children"]) > 0


def test_flatten_trace_deep_nesting():
    from app.decoder import flatten_trace

    trace = {
        "type": "CALL",
        "from": "0x" + "a" * 40,
        "to": "0x" + "b" * 40,
        "input": "0x",
        "gas": "0x1",
        "gasUsed": "0x1",
        "calls": [
            {
                "type": "CALL",
                "from": "0x" + "c" * 40,
                "to": "0x" + "d" * 40,
                "input": "0x",
                "gas": "0x1",
                "gasUsed": "0x1",
                "calls": [
                    {
                        "type": "CALL",
                        "from": "0x" + "e" * 40,
                        "to": "0x" + "f" * 40,
                        "input": "0x",
                        "gas": "0x1",
                        "gasUsed": "0x1",
                    }
                ],
            }
        ],
    }
    flat = flatten_trace(trace)
    assert len(flat) == 3
    assert flat[0]["depth"] == 0
    assert flat[1]["depth"] == 1
    assert flat[2]["depth"] == 2


def test_flatten_trace_list_input():
    from app.decoder import flatten_trace

    A, B = "0x" + "a" * 40, "0x" + "b" * 40
    C, D = "0x" + "c" * 40, "0x" + "d" * 40
    traces = [
        {"type": "CALL", "from": A, "to": B, "input": "0x", "gas": "0x1", "gasUsed": "0x1"},
        {"type": "CALL", "from": C, "to": D, "input": "0x", "gas": "0x1", "gasUsed": "0x1"},
    ]
    flat = flatten_trace(traces)
    assert len(flat) == 2
    assert flat[0]["depth"] == 0
    assert flat[1]["depth"] == 0


# ── extract_calls ──────────────────────────────────────────────────


def test_extract_calls_with_trace():
    from app.decoder import extract_calls

    trace = [{"depth": 0, "type": "CALL"}]
    assert extract_calls({}, trace) is trace


def test_extract_calls_without_trace_empty():
    from app.decoder import extract_calls

    assert extract_calls({"raw": "{}"}, None) == []


def test_extract_calls_fallback_from_raw():
    import json

    from app.decoder import extract_calls

    tx = {
        "from_addr": "0x1111111111111111111111111111111111111111",
        "raw": json.dumps(
            {
                "calls": [
                    {
                        "to": "0x2222222222222222222222222222222222222222",
                        "value": "0x0",
                        "data": "0x",
                    },
                    {
                        "to": "0x3333333333333333333333333333333333333333",
                        "value": "0xa",
                        "data": "0x",
                    },
                ]
            }
        ),
    }
    result = extract_calls(tx, None)
    assert len(result) == 2
    assert result[0]["to"] == "0x2222222222222222222222222222222222222222"
    assert result[0]["depth"] == 0
    assert result[1]["value"] == 10


# ── extract_balance_changes ─────────────────────────────────────────


def test_extract_balance_changes_simple_transfer():
    from app.decoder import extract_balance_changes

    A, B = (
        "0x1111111111111111111111111111111111111111",
        "0x2222222222222222222222222222222222222222",
    )
    changes = extract_balance_changes({"logs": [_transfer_log(A, B, 1000)]}, {})
    assert len(changes) == 2
    assert changes[0]["address"].lower() == A.lower()
    assert changes[0]["change"] == "-1000"
    assert changes[0]["is_fee"] is False
    assert changes[1]["address"].lower() == B.lower()
    assert changes[1]["change"] == "+1000"


def test_extract_balance_changes_fee_marking():
    from tempo.constants import FEE_MANAGER_ADDRESS

    from app.decoder import extract_balance_changes

    fee_token = "0x20C0000000000000000000000000000000000000"
    user = "0x1111111111111111111111111111111111111111"
    logs = [_transfer_log(user, FEE_MANAGER_ADDRESS, 500, contract=fee_token)]
    tx = {"fee_token": fee_token, "fee_amount": "500"}
    changes = extract_balance_changes({"logs": logs}, tx)
    receiver = [c for c in changes if c["address"].lower() == FEE_MANAGER_ADDRESS.lower()]
    assert len(receiver) == 1
    assert receiver[0]["is_fee"] is True
    assert receiver[0]["change_type"] == "fee"


def test_extract_balance_changes_no_fee_when_no_fee_token():
    from tempo.constants import FEE_MANAGER_ADDRESS

    from app.decoder import extract_balance_changes

    logs = [_transfer_log("0x1111111111111111111111111111111111111111", FEE_MANAGER_ADDRESS, 500)]
    for c in extract_balance_changes({"logs": logs}, {}):
        assert c["is_fee"] is False


def test_extract_balance_changes_multiple_tokens():
    from app.decoder import extract_balance_changes

    token_a, token_b = "0x" + "a" * 40, "0x" + "b" * 40
    user, pool = "0x" + "1" * 40, "0x" + "2" * 40
    changes = extract_balance_changes(
        {
            "logs": [
                _transfer_log(user, pool, 1000, token_a),
                _transfer_log(pool, user, 500, token_b),
            ]
        },
        {},
    )
    assert len(changes) == 4
    tokens = set(c["token"].lower() for c in changes)
    assert token_a.lower() in tokens
    assert token_b.lower() in tokens


def test_decode_function_call_approve():
    from app.decoder import decode_function_call

    # approve(spender=0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, amount=1)
    data = (
        "0x095ea7b3"
        "000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        "0000000000000000000000000000000000000000000000000000000000000001"
    )
    result = decode_function_call(data)
    assert result is not None
    assert result.name == "approve"


def test_decode_function_call_mint():
    from app.decoder import decode_function_call

    # mint(to=0x0000000000000000000000000000000000000001, amount=100)
    data = (
        "0x40c10f19"
        "0000000000000000000000000000000000000000000000000000000000000001"
        "0000000000000000000000000000000000000000000000000000000000000064"
    )
    result = decode_function_call(data)
    assert result is not None
    assert result.name == "mint"
    assert len(result.params) == 2


# ── extract_state_changes ──────────────────────────────────────────


def test_extract_state_changes_empty():
    from app.decoder import extract_state_changes

    assert extract_state_changes({}) == []
    assert extract_state_changes({"logs": []}) == []


def test_extract_state_changes_with_state_diff():
    from app.decoder import extract_state_changes

    receipt = {"stateDiff": {"0x" + "a" * 40: {"0x00" * 32: {"before": "0x0", "after": "0x1"}}}}
    result = extract_state_changes(receipt)
    assert len(result) == 1
    assert "a" in result[0]["contract"].lower()


# ── decode_event ───────────────────────────────────────────────────


def test_decode_transfer_event():
    from eth_utils import keccak

    from app.decoder import decode_event

    topic = "0x" + keccak(b"Transfer(address,address,uint256)").hex()
    log = {
        "topics": [
            topic,
            "0x0000000000000000000000001111111111111111111111111111111111111111",
            "0x0000000000000000000000002222222222222222222222222222222222222222",
        ],
        "data": "0x00000000000000000000000000000000000000000000000000000000000003e8",
        "address": "0x" + "c" * 40,
        "logIndex": "0x1",
    }
    decoded = decode_event(log)
    assert decoded is not None
    assert decoded.name == "Transfer"
    assert len(decoded.params) == 3


def test_decode_approval_event():
    from eth_utils import keccak

    from app.decoder import decode_event

    topic = "0x" + keccak(b"Approval(address,address,uint256)").hex()
    log = {
        "topics": [
            topic,
            "0x0000000000000000000000001111111111111111111111111111111111111111",
            "0x0000000000000000000000002222222222222222222222222222222222222222",
        ],
        "data": "0x00000000000000000000000000000000000000000000000000000000000003e8",
        "address": "0x" + "c" * 40,
        "logIndex": "0x2",
    }
    decoded = decode_event(log)
    assert decoded is not None
    assert decoded.name == "Approval"


def test_decode_event_no_topics():
    from app.decoder import decode_event

    assert decode_event({"data": "0x"}) is None
    assert decode_event({}) is None


# ── decode_function_call ───────────────────────────────────────────


def test_decode_function_call_transfer():
    from app.decoder import decode_function_call

    data = "0xa9059cbb000000000000000000000000060b0fb0be9d90557577b3aee480711067149ff000000000000000000000000000000000000000000000000000000000000003e8"
    result = decode_function_call(data)
    assert result is not None
    assert result.name == "transfer"
    assert result.params[0].name == "to"
    assert result.params[1].value == "1000"


def test_decode_function_call_burn():
    from app.decoder import decode_function_call

    data = "0x42966c680000000000000000000000000000000000000000000000000000000000000001"
    result = decode_function_call(data)
    assert result is not None
    assert result.name == "burn"


def test_decode_function_call_total_supply():
    from app.decoder import decode_function_call

    data = "0x18160ddd"
    result = decode_function_call(data)
    assert result is not None
    assert result.name == "totalSupply"


def test_decode_function_call_empty():
    from app.decoder import decode_function_call

    assert decode_function_call("0x") is None
    assert decode_function_call("") is None


def test_decode_function_call_unknown():
    from app.decoder import decode_function_call

    data = "0xdeadbeef0000000000000000000000000000000000000000000000000000000000000001"
    result = decode_function_call(data)
    assert result is not None
    assert result.name is None
    assert result.selector == "0xdeadbeef"
