"""Independent AV1 symbol (arithmetic) decoder — AV1 spec §8.2.

Ported by hand from tpt-kinetix-av1/src/entropy.rs::SymbolDecoder, which
implements §8.2.2 init_symbol / §8.2.3 read_bool / §8.2.5 read_literal /
§8.2.6 read_symbol verbatim. The control flow here is written fresh from the
spec text (so it catches context-derivation / read-order bugs), while the
probability tables it consumes come from cdf_tables_gen (exported from the
Rust crate — see that module's header for the independence caveat).

A CDF array is length N+1 for an N-symbol alphabet: cdf[N-1] == 32768 and
cdf[N] is the adaptation counter (saturating at 32). All arithmetic is u32
with the spec's fixed constants.
"""

EC_PROB_SHIFT = 6
EC_MIN_PROB = 4

# Sentinel "cdf" for a boolean (read_bool builds one fresh each call, so its
# adaptation is discarded — only the decoded value matters for the trace).
_BOOL_CDF = [1 << 14, 1 << 15, 0]


def _floor_log2(x: int) -> int:
    # x >= 1
    s = 0
    while x != 0:
        x >>= 1
        s += 1
    return s - 1


class SymbolDecoder:
    """MSB-first arithmetic decoder over a byte buffer, per §8.2.2."""

    def __init__(self, data: bytes, bit_offset: int = 0):
        self.data = data
        self.bit_pos = bit_offset
        self.symbol_value = 0
        self.symbol_range = 1 << 15
        self.symbol_max_bits = 0
        self.trace = []  # filled by read_symbol when tracing is on
        remaining_bits = len(data) * 8 - bit_offset
        if remaining_bits < 0:
            remaining_bits = 0
        num_bits = min(remaining_bits, 15)
        buf = self._f(num_bits)
        padded_buf = buf << (15 - num_bits)
        self.symbol_value = ((1 << 15) - 1) ^ padded_buf
        self.symbol_range = 1 << 15
        self.symbol_max_bits = remaining_bits - 15

    @classmethod
    def from_raw_state(cls, data: bytes, bit_offset: int, symbol_range: int,
                        symbol_value: int, symbol_max_bits: int) -> "SymbolDecoder":
        """Resume mid-tile from Kinetix's exact arithmetic-coder state.

        `__init__`'s `init_symbol`-style construction always forces
        `symbol_range = 1 << 15`, which is only correct for a genuinely fresh
        stream (tile start). Mid-tile, `symbol_range` is only guaranteed to be
        in `[1 << 15, 1 << 16)` after renormalization (the renorm shift lands
        wherever `floor_log2(range)` was, not necessarily back to exactly
        `1 << 15`) — re-deriving it from raw bytes at an arbitrary bit offset
        silently assumes the wrong value whenever the true range isn't
        exactly `32768` at that instant, corrupting every read from there on
        even though the decoder algorithm and CDF tables are both correct.
        This constructor bypasses that assumption by taking the real
        `symbol_range`/`symbol_value`/`symbol_max_bits` triple directly (see
        `SymbolDecoder::raw_state` in `entropy.rs`).
        """
        dec = cls.__new__(cls)
        dec.data = data
        dec.bit_pos = bit_offset
        dec.symbol_range = symbol_range
        dec.symbol_value = symbol_value
        dec.symbol_max_bits = symbol_max_bits
        dec.trace = []
        return dec

    def _read_bit(self) -> int:
        byte_idx = self.bit_pos // 8
        if byte_idx < len(self.data):
            shift = 7 - (self.bit_pos % 8)
            bit = (self.data[byte_idx] >> shift) & 1
        else:
            bit = 0
        self.bit_pos += 1
        return bit

    def _f(self, n: int) -> int:
        x = 0
        for _ in range(n):
            x = (x << 1) | self._read_bit()
        return x

    def read_symbol(self, cdf: list) -> int:
        n = len(cdf) - 1
        assert n >= 2, "cdf must describe at least 2 symbols"
        assert cdf[n - 1] == 1 << 15, "cdf[N-1] must be 32768"
        bit_pos_before = self.bit_pos

        cur = self.symbol_range
        prev = 0
        symbol = 0
        while True:
            prev = cur
            f = (1 << 15) - cdf[symbol]
            cur = ((self.symbol_range >> 8) * (f >> EC_PROB_SHIFT)) >> (
                7 - EC_PROB_SHIFT
            )
            cur += EC_MIN_PROB * (n - symbol - 1)
            if self.symbol_value >= cur:
                break
            symbol += 1

        self.symbol_range = prev - cur
        self.symbol_value -= cur

        bits = 15 - _floor_log2(self.symbol_range)
        self.symbol_range <<= bits
        num_bits = min(bits, max(0, self.symbol_max_bits))
        new_data = self._f(num_bits)
        padded_data = new_data << (bits - num_bits)
        self.symbol_value = padded_data ^ (((self.symbol_value + 1) << bits) - 1)
        self.symbol_max_bits -= bits

        count = cdf[n]
        rate = 3 + (1 if count > 15 else 0) + (1 if count > 31 else 0) + min(_floor_log2(n), 2)
        tmp = 0
        for i in range(n - 1):
            if i == symbol:
                tmp = 1 << 15
            c = cdf[i]
            if tmp < c:
                cdf[i] = c - ((c - tmp) >> rate)
            else:
                cdf[i] = c + ((tmp - c) >> rate)
        if cdf[n] < 32:
            cdf[n] += 1

        self.trace.append(
            {
                "n": n,
                "value": symbol,
                "before": bit_pos_before,
                "after": self.bit_pos,
                "rng": self.symbol_range,
                "val": self.symbol_value,
            }
        )
        return symbol

    def read_bool(self) -> bool:
        return self.read_symbol(list(_BOOL_CDF)) == 1

    def read_literal(self, n: int) -> int:
        x = 0
        for _ in range(n):
            x = (x << 1) | (1 if self.read_bool() else 0)
        return x
