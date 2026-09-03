#!/usr/bin/env bash
#
# Fetch a curated subset of the ITU-T H.264.1 (H.264 conformance) bitstream
# suite into tpt-kinetix-h264/tests/fixtures/itu/<CLIP>/ for the
# `itu_conformance` integration test.
#
# Source: https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance/
#   AVCv1/  — original AVC (Baseline / Main / Extended) conformance streams
#   FRExt/  — Fidelity Range Extensions (High profile, 8x8 transform, 4:2:2/4:4:4)
#
# Each archive holds a bitstream (.264 / .jsv), a reference decoded YUV
# (*_rec.yuv or *.yuv), a readme, and often a huge bit-level trace.txt which we
# discard. The reference YUV is the *normative* output — the test compares our
# decode byte-for-byte against it (no third-party decoder in the loop).
#
# Fixtures are large (~1 GB for this list) and git-ignored. Re-running skips
# clips already present. Set CLIPS="A B C" to fetch a specific subset, or
# GROUP=frext to fetch only the FRExt list.
#
# Usage:
#   tools/fetch-h264-conformance.sh
#   CLIPS="BA1_Sony_D CABA1_Sony_D" tools/fetch-h264-conformance.sh

set -euo pipefail

BASE="https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance"
DEST="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/tpt-kinetix-h264/tests/fixtures/itu"

# clip name -> suite subdir. Curated to cover exactly what
# `capabilities().pixel_exact == true` claims, plus a few negative clips.
declare -A CLIP_GROUP=(
  # --- progressive CAVLC (Baseline / Main) ---
  [BA1_Sony_D]=AVCv1        # I-only, CAVLC, QCIF (Foreman)
  [BA1_FT_C]=AVCv1          # I/P, CAVLC
  [BA2_Sony_F]=AVCv1        # I/P, CAVLC
  [BA3_SVA_C]=AVCv1         # I/P, CAVLC
  [NL1_Sony_D]=AVCv1        # no loop filter
  [NL2_Sony_H]=AVCv1
  [NL3_SVA_E]=AVCv1
  [MIDR_MW_D]=AVCv1         # multiple IDR
  [CI1_FT_B]=AVCv1          # constrained intra pred
  [CVPCMNL1_SVA_C]=AVCv1    # I_PCM macroblocks
  [CVPCMNL2_SVA_C]=AVCv1
  [SVA_NL2_E]=AVCv1
  [MPS_MW_A]=AVCv1          # multiple slice groups (negative: unsupported)

  # --- progressive CABAC (I / I-P / I-P-B) ---
  [CABA1_Sony_D]=AVCv1      # I-only CABAC
  [CABA2_Sony_E]=AVCv1     # I/P CABAC
  [CABA3_Sony_C]=AVCv1     # I/P/B CABAC
  [CANL1_Sony_E]=AVCv1
  [CANL2_Sony_E]=AVCv1
  [CANL3_Sony_C]=AVCv1
  [CACQP3_Sony_D]=AVCv1
  [CABAST3_Sony_E]=AVCv1
  [CABASTBR3_Sony_B]=AVCv1
  [CVBS3_Sony_C]=AVCv1
  [CABACI3_Sony_B]=AVCv1    # 4 slices/picture (negative: unsupported)

  # --- PAFF (picture-adaptive field/frame) ---
  [CVPA1_TOSHIBA_B]=AVCv1
  [CAPA1_TOSHIBA_B]=AVCv1
  [FM1_BT_B]=AVCv1
  [FM1_FT_E]=AVCv1
  [CVFI1_Sony_D]=AVCv1
  [cabac_mot_picaff0_full]=AVCv1
  [cavlc_mot_picaff0_full_B]=AVCv1
  [Sharp_MP_PAFF_1r2]=AVCv1

  # --- MBAFF (macroblock-adaptive field/frame) ---
  [CAMA1_Sony_C]=AVCv1      # MBAFF CABAC I, 720x480
  [CAMA1_TOSHIBA_B]=AVCv1
  [CAMA3_Sand_E]=AVCv1
  [CAMANL1_TOSHIBA_B]=AVCv1
  [CAMANL3_Sand_E]=AVCv1
  [cama1_vtc_c]=AVCv1
  [cama2_vtc_b]=AVCv1
  [CANLMA2_Sony_C]=AVCv1
  [CANLMA3_Sony_C]=AVCv1
  [cabac_mot_mbaff0_full]=AVCv1
  [cavlc_mot_mbaff0_full_B]=AVCv1
  [CAPAMA3_Sand_F]=AVCv1    # PAFF + MBAFF
  [CAMP_MOT_MBAFF_L30]=AVCv1

  # --- FRExt: High profile 4:2:0, 8x8 transform ---
  [HCHP1_HHI_B]=FRExt       # High, I/P/B, hierarchical GOP
  [HCHP2_HHI_A]=FRExt
  [HCHP3_HHI_A]=FRExt
  [HCBP1_HHI_A]=FRExt       # High, Baseline-like structure
  [HCBP2_HHI_A]=FRExt
  [HCMP1_HHI_A]=FRExt
  [FRExt1_Panasonic_D]=FRExt
  [FRExt2_Panasonic_C]=FRExt
  [FRExt3_Panasonic_E]=FRExt
  [FRExt4_Panasonic_B]=FRExt
  [freh1_b]=FRExt
  [freh2_b]=FRExt
  [freh7_b]=FRExt
  [FREXT01_JVC_D]=FRExt
  [FREXT02_JVC_C]=FRExt
  [HCAFR1_HHI_C]=FRExt      # High + frame coding
  [HCAFF1_HHI_B]=FRExt      # High + PAFF
  [HPCA_BRCM_C]=FRExt
  [HPCANL_BRCM_C]=FRExt
  [HVLCFI0_Sony_B]=FRExt

  # --- FRExt negatives: non-4:2:0 chroma (decoder rejects pixel-exact) ---
  [Hi422FR1_SONY_A]=FRExt   # 4:2:2
  [Hi422FREXT16_SONY_A]=FRExt
)

mkdir -p "$DEST"

names=()
if [[ -n "${CLIPS:-}" ]]; then
  read -r -a names <<<"$CLIPS"
else
  for k in "${!CLIP_GROUP[@]}"; do
    if [[ -z "${GROUP:-}" || "${CLIP_GROUP[$k],,}" == "${GROUP,,}" ]]; then
      names+=("$k")
    fi
  done
fi

IFS=$'\n' names=($(sort <<<"${names[*]}")); unset IFS

ok=0; skip=0; fail=0
for name in "${names[@]}"; do
  group="${CLIP_GROUP[$name]:-AVCv1}"
  outdir="$DEST/$name"
  if [[ -d "$outdir" ]] && compgen -G "$outdir/*.yuv" >/dev/null; then
    echo "  skip  $name (present)"
    skip=$((skip + 1))
    continue
  fi
  url="$BASE/$group/$name.zip"
  tmp="$(mktemp -d)"
  echo "  fetch $name  <- $url"
  if ! curl -fsSL --max-time 300 -o "$tmp/c.zip" "$url"; then
    echo "  FAIL  $name (download)"
    fail=$((fail + 1))
    rm -rf "$tmp"
    continue
  fi
  mkdir -p "$outdir"
  # Extract everything except the multi-MB trace files.
  unzip -o -q "$tmp/c.zip" -d "$outdir" -x "*trace*" "*.txt.gz" || true
  # Some archives still ship the trace as *_trace.txt; drop anything huge.
  find "$outdir" -type f -name '*trace*' -delete 2>/dev/null || true
  rm -rf "$tmp"
  if compgen -G "$outdir/*.yuv" >/dev/null; then
    echo "  ok    $name"
    ok=$((ok + 1))
  else
    echo "  FAIL  $name (no reference YUV in archive)"
    fail=$((fail + 1))
  fi
done

echo
echo "fetched $ok, skipped $skip, failed $fail  ->  $DEST"
[[ $fail -eq 0 ]]
