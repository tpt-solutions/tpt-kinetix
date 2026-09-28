"""Minimal H.264 header scan for CAPA1_TOSHIBA_B.264.

Extracts per-NAL: nal_ref_idc, type, and for slice NALs the header fields up to
the POC group: frame_num, field_pic_flag, bottom_field_flag, pic_order_cnt_lsb,
delta_pic_order_cnt_bottom. Enough to answer: do the frame-coded pictures carry
a bottom delta, and what POCs does 8.2.1 derive per picture?
"""
import sys

path = sys.argv[1] if len(sys.argv) > 1 else (
    "tpt-kinetix-h264/tests/fixtures/itu/CAPA1_TOSHIBA_B/CAPA1_TOSHIBA_B.264")
data = open(path, "rb").read()


class BR:
    def __init__(self, b):
        self.b = b
        self.p = 0

    def u(self, n):
        v = 0
        for _ in range(n):
            byte = self.b[self.p >> 3]
            bit = (byte >> (7 - (self.p & 7))) & 1
            v = (v << 1) | bit
            self.p += 1
        return v

    def ue(self):
        z = 0
        while self.u(1) == 0:
            z += 1
            if z > 32:
                raise ValueError("ue too long")
        return (1 << z) - 1 + (self.u(z) if z else 0)

    def se(self):
        k = self.ue()
        return (k + 1) // 2 if k % 2 else -(k // 2)


def nals(buf):
    i = 0
    out = []
    while True:
        j = buf.find(b"\x00\x00\x01", i)
        if j < 0:
            break
        start = j + 3
        k = buf.find(b"\x00\x00\x01", start)
        end = k if k >= 0 else len(buf)
        # strip trailing zero padding of previous NAL
        e = end
        while e > start and buf[e - 1] == 0:
            e -= 1
        out.append(buf[start:e])
        i = j + 3
    return out


def unescape(rbsp):
    out = bytearray()
    i = 0
    while i < len(rbsp):
        if i + 2 < len(rbsp) and rbsp[i] == 0 and rbsp[i + 1] == 0 and rbsp[i + 2] == 3:
            out += b"\x00\x00"
            i += 3
        else:
            out.append(rbsp[i])
            i += 1
    return bytes(out)


sps = None
ppss = {}
rows = []
for n in nals(data):
    if not n:
        continue
    hdr = n[0]
    nal_ref_idc = hdr >> 5
    t = hdr & 0x1F
    if t == 7:
        r = BR(unescape(n[1:]))
        r.ue()  # profile_idc is u(8) — not ue! redo below
    # SPS parse (profile_idc is u(8), constraints u(8), level u(8), then ue's)
    if t == 7:
        body = unescape(n[1:])
        r = BR(body)
        profile = r.u(8)
        cons = r.u(8)
        level = r.u(8)
        sps_id = r.ue()
        if profile in (100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135):
            chroma = r.ue()
            if chroma == 3:
                r.u(1)
            r.ue()
            r.ue()
            r.u(1)
            if r.u(1):
                for i in range(8):
                    if r.u(1):
                        if i < 6:
                            for _ in range(16):
                                r.se()
                        else:
                            for _ in range(64):
                                r.se()
        log2_mfn_m4 = r.ue()
        pct = r.ue()
        if pct == 0:
            log2_poc_lsb_m4 = r.ue()
        elif pct == 1:
            r.u(1)
            off = r.se()
            nsp = r.ue()
            for _ in range(min(nsp, 255)):
                off2 = r.se()
            ntb = r.ue()
            for _ in range(min(ntb, 255)):
                r.se()
        gaps = r.u(1)
        if gaps:
            for _ in range(r.ue() + 1):
                r.ue()
        fmo = r.u(1)
        fmb = r.u(1)
        if not fmb:
            mbaff = r.u(1)
        direct8 = r.u(1)
        sps = dict(sps_id=sps_id, log2_mfn_m4=log2_mfn_m4, pct=pct,
                   log2_poc_lsb_m4=log2_poc_lsb_m4 if pct == 0 else None,
                   frame_mbs_only=fmb)
        print("SPS:", sps)
    elif t == 8:
        body = unescape(n[1:])
        r = BR(body)
        pps_id = r.ue()
        sps_id = r.ue()
        ent = r.u(1)
        bof = r.u(1)
        ppss[pps_id] = dict(ent=ent, bof=bof)
        print("PPS:", ppss[pps_id])
    elif t in (1, 5):
        body = unescape(n[1:])
        r = BR(body)
        first_mb = r.ue()
        st = r.ue()
        pps_id = r.ue()
        if sps is None:
            continue
        l2 = sps["log2_mfn_m4"] + 4
        frame_num = r.u(l2)
        fmb = sps["frame_mbs_only"]
        field_pic = False
        bottom = False
        if not fmb:
            field_pic = bool(r.u(1))
            if field_pic:
                bottom = bool(r.u(1))
        if nal_ref_idc == 0:
            continue
        if nal_ref_idc == 0 and t == 5:
            pass
        idr = t == 5
        if idr:
            idr_pic_id = r.ue()
        delta_bottom = None
        poc_lsb = None
        if sps["pct"] == 0:
            bits = sps["log2_poc_lsb_m4"] + 4
            poc_lsb = r.u(bits)
            p = ppss.get(pps_id, {})
            if p.get("bof") and not field_pic:
                delta_bottom = r.se()
        rows.append((t, nal_ref_idc, first_mb, st, pps_id, frame_num,
                     field_pic, bottom, poc_lsb, delta_bottom))

print()
print("slice NALs:", len(rows))
prev = None
for (t, idc, fmb_, st, pps, fn, fp, bo, lsb, db) in rows:
    kind = {7: "IDR"}.get(t, "S")
    tag = "FIELD" if fp else "FRAME"
    print("type=%d ref=%d first_mb=%-4d st=%-2d pps=%d fn=%-3d %-5s bot=%d poc_lsb=%-5s delta_b=%s"
          % (t, idc, fmb_, st, pps, fn, tag, bo, lsb, db))
