# Coding conventions

## Error handling: never swallow exceptions

**Rule:** `except Exception: pass` is forbidden in application code. Every exception handler must:

1. Catch only the specific exception type(s) you expect (`ValueError`, `KeyError`, `TimeoutError`, etc.)
2. Handle the error meaningfully — log it, fall back to a sensible default, or re-raise
3. **Never** silence an exception you don't understand

```python
# BAD — hides every possible error including bugs:
try:
    checksummed = to_checksum_address(addr)
except Exception:
    pass

# GOOD — only catches what you expect:
try:
    checksummed = to_checksum_address(addr)
except (ValueError, TypeError):
    pass
```

### Exceptions to the rule

A few places may use broad `except` when there are genuinely many possible failure modes and the handler does something useful with the error:

| File | Location | Why acceptable |
|------|----------|---------------|
| `app/database.py` | `get_session()` | Rolls back transaction on any error — necessary for DB consistency |
| `app/decoder.py` | `_decode_params`, `_extract_param_names` | ABI data can be malformed in many ways; returns `[]` gracefully |
| `app/indexer.py` | `run_forever` | Background worker must never crash; logs the error and continues |

### Why this matters

Silent exception swallowing is the #1 cause of hard-to-debug 500 errors in this codebase. Every time you write `except Exception: pass`, you risk hiding a real bug that would be obvious from a traceback.

If you're tempted to write `except Exception: pass`:
1. Run the code and see what exception actually occurs
2. Catch that specific type instead
3. If the handler truly does nothing useful, let the error propagate so the operator sees it

## Imports: module-level only

All imports must be at the top of the file, never inside functions or methods (with one documented exception).

### Exception

`app/rpc.py` has lazy imports inside `fetch_and_cache_block` and `fetch_transaction_with_receipt` to break a circular dependency: `database.py → tokens.py → rpc.py → database.py`.

Every other file must use module-level imports.

## Tests

Inline imports inside test functions are acceptable (standard pytest practice). Short uppercase variable names (`A`, `B`, `addr_a`) are also fine.
